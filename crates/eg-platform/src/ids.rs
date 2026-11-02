//! ULID 生成（`SPEC-platform.md` §4.5）。
//!
//! 要求：进程内单例、同毫秒单调自增、最近毫秒持久化、时钟回拨不倒退。
//! 本模块只使用 `domain` 的 [`Ulid`] 与各 id 新类型，不另造 id 类型。
//!
//! 设计：
//! - 时间戳取「注入时钟」与「已持久化毫秒」的较大值；回拨时沿用历史最大毫秒并记警告。
//! - 同毫秒内随机部分 +1 保证严格递增；溢出则进位到下一毫秒。
//! - 最近毫秒写入状态文件（每次毫秒变化写一次，避免每次生成的 IO 抖动）。

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use eg_domain::{
    ClassroomId, ComparatorTraceId, ConsentId, GradeId, ModelCallId, PageId, PaperId, QuestionId,
    RecognitionId, ReviewActionId, RubricId, RubricItemId, SegmentId, StudentId, SubmissionId,
    TaskId, Ulid,
};
use serde::{Deserialize, Serialize};

use crate::clock::SystemClock;
use crate::error::{PlatformError, Result};
use crate::paths::ULID_STATE_FILE;

/// ULID 时间戳位宽（毫秒）。
const TIMESTAMP_BITS: u32 = 48;
/// ULID 随机部分位宽。
const RANDOM_BITS: u32 = 80;
/// 随机部分掩码。
const RANDOM_MASK: u128 = (1u128 << RANDOM_BITS) - 1;

/// 持久化状态（JSON 文本，便于人工排查）。
///
/// 同时记录最近毫秒与最近 ULID：前者用于时钟回拨保护，后者保证跨重启严格递增
/// （同毫秒重启时随机部分可能更小，仅有毫秒不足以保证不倒退）。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedState {
    /// 历史最大毫秒（Unix 毫秒）。
    last_ms: u64,
    /// 最近生成的 ULID（26 字符）；旧版本状态文件可能缺失。
    #[serde(default)]
    last_ulid: Option<String>,
}

/// 进程内 ULID 工厂（同毫秒严格递增）。
pub struct UlidFactory {
    state: Mutex<FactoryState>,
    clock: Arc<dyn eg_domain::Clock>,
    persist_path: Option<PathBuf>,
}

#[derive(Debug)]
struct FactoryState {
    /// 最近生成的 ULID（`None` 表示尚未生成）。
    last: Option<Ulid>,
    /// 历史最大毫秒（时钟回拨保护）。
    floor_ms: u64,
    /// 已持久化的毫秒（避免重复写盘）。
    persisted_ms: Option<u64>,
    /// 已持久化的 ULID。
    persisted_ulid: Option<Ulid>,
    /// xorshift 状态（随机部分）。
    rng: u64,
}

impl std::fmt::Debug for UlidFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UlidFactory")
            .field("persist_path", &self.persist_path)
            .finish_non_exhaustive()
    }
}

impl UlidFactory {
    /// 创建无持久化的工厂（测试/临时使用）。
    #[must_use]
    pub fn new(clock: Arc<dyn eg_domain::Clock>) -> Self {
        Self {
            state: Mutex::new(FactoryState {
                last: None,
                floor_ms: 0,
                persisted_ms: None,
                persisted_ulid: None,
                rng: seed_rng(),
            }),
            clock,
            persist_path: None,
        }
    }

    /// 创建带毫秒持久化的工厂（生产使用）。
    ///
    /// # Errors
    ///
    /// 状态文件存在但无法读取/解析时返回错误（不静默重置，避免 id 回退）。
    pub fn with_persistence(
        clock: Arc<dyn eg_domain::Clock>,
        path: impl AsRef<Path>,
    ) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let (floor_ms, last) = read_state(&path)?;
        Ok(Self {
            state: Mutex::new(FactoryState {
                last,
                floor_ms,
                persisted_ms: None,
                persisted_ulid: None,
                rng: seed_rng(),
            }),
            clock,
            persist_path: Some(path),
        })
    }

    /// 生成下一个 ULID（严格单调递增）。
    ///
    /// # Errors
    ///
    /// 锁中毒或状态文件写入失败时返回错误。
    pub fn next_ulid(&self) -> Result<Ulid> {
        let now_ms = self.current_ms();
        let mut state = self
            .state
            .lock()
            .map_err(|_| PlatformError::Internal("ULID 锁中毒".to_owned()))?;

        let mut timestamp_ms = now_ms.max(state.floor_ms);
        if let Some(last) = state.last {
            timestamp_ms = timestamp_ms.max(last.timestamp_ms());
        }

        let candidate = match state.last {
            Some(last) if last.timestamp_ms() == timestamp_ms => {
                let random = last.random().wrapping_add(1) & RANDOM_MASK;
                if random == 0 {
                    // 随机部分回绕：进位到下一毫秒。
                    timestamp_ms = timestamp_ms.saturating_add(1);
                    Ulid::from_parts(timestamp_ms, state.next_random())
                } else {
                    Ulid::from_parts(timestamp_ms, random)
                }
            }
            _ => Ulid::from_parts(timestamp_ms, state.next_random()),
        };

        // 兜底保证严格递增（例如回拨后 floor 与 last 组合仍可能不增）。
        let ulid = match state.last {
            Some(last) if candidate <= last => last
                .checked_increment()
                .ok_or_else(|| PlatformError::Internal("ULID 递增溢出".to_owned()))?,
            _ => candidate,
        };

        state.last = Some(ulid);
        state.floor_ms = state.floor_ms.max(ulid.timestamp_ms());
        drop(state);

        self.persist(ulid)?;
        Ok(ulid)
    }

    /// 生成任务 id。
    ///
    /// # Errors
    ///
    /// 同 [`UlidFactory::next_ulid`]。
    pub fn next_task_id(&self) -> Result<TaskId> {
        self.next_ulid().map(TaskId::from_ulid)
    }

    /// 生成提交 id。
    ///
    /// # Errors
    ///
    /// 同 [`UlidFactory::next_ulid`]。
    pub fn next_submission_id(&self) -> Result<SubmissionId> {
        self.next_ulid().map(SubmissionId::from_ulid)
    }

    /// 生成授权 id。
    ///
    /// # Errors
    ///
    /// 同 [`UlidFactory::next_ulid`]。
    pub fn next_consent_id(&self) -> Result<ConsentId> {
        self.next_ulid().map(ConsentId::from_ulid)
    }

    /// 生成判分记录 id。
    ///
    /// # Errors
    ///
    /// 同 [`UlidFactory::next_ulid`]。
    pub fn next_grade_id(&self) -> Result<GradeId> {
        self.next_ulid().map(GradeId::from_ulid)
    }

    /// 生成模型调用 id。
    ///
    /// # Errors
    ///
    /// 同 [`UlidFactory::next_ulid`]。
    pub fn next_model_call_id(&self) -> Result<ModelCallId> {
        self.next_ulid().map(ModelCallId::from_ulid)
    }

    /// 生成页面 id。
    ///
    /// # Errors
    ///
    /// 同 [`UlidFactory::next_ulid`]。
    pub fn next_page_id(&self) -> Result<PageId> {
        self.next_ulid().map(PageId::from_ulid)
    }

    /// 生成识别记录 id。
    ///
    /// # Errors
    ///
    /// 同 [`UlidFactory::next_ulid`]。
    pub fn next_recognition_id(&self) -> Result<RecognitionId> {
        self.next_ulid().map(RecognitionId::from_ulid)
    }

    /// 生成区域 id。
    ///
    /// # Errors
    ///
    /// 同 [`UlidFactory::next_ulid`]。
    pub fn next_segment_id(&self) -> Result<SegmentId> {
        self.next_ulid().map(SegmentId::from_ulid)
    }

    /// 生成班级 id。
    ///
    /// # Errors
    ///
    /// 同 [`UlidFactory::next_ulid`]。
    pub fn next_classroom_id(&self) -> Result<ClassroomId> {
        self.next_ulid().map(ClassroomId::from_ulid)
    }

    /// 生成学生 id。
    ///
    /// # Errors
    ///
    /// 同 [`UlidFactory::next_ulid`]。
    pub fn next_student_id(&self) -> Result<StudentId> {
        self.next_ulid().map(StudentId::from_ulid)
    }

    /// 生成试卷 id。
    ///
    /// # Errors
    ///
    /// 同 [`UlidFactory::next_ulid`]。
    pub fn next_paper_id(&self) -> Result<PaperId> {
        self.next_ulid().map(PaperId::from_ulid)
    }

    /// 生成题目 id。
    ///
    /// # Errors
    ///
    /// 同 [`UlidFactory::next_ulid`]。
    pub fn next_question_id(&self) -> Result<QuestionId> {
        self.next_ulid().map(QuestionId::from_ulid)
    }

    /// 生成规则 id。
    ///
    /// # Errors
    ///
    /// 同 [`UlidFactory::next_ulid`]。
    pub fn next_rubric_id(&self) -> Result<RubricId> {
        self.next_ulid().map(RubricId::from_ulid)
    }

    /// 生成评分项 id。
    ///
    /// # Errors
    ///
    /// 同 [`UlidFactory::next_ulid`]。
    pub fn next_rubric_item_id(&self) -> Result<RubricItemId> {
        self.next_ulid().map(RubricItemId::from_ulid)
    }

    /// 生成比较轨迹 id。
    ///
    /// # Errors
    ///
    /// 同 [`UlidFactory::next_ulid`]。
    pub fn next_comparator_trace_id(&self) -> Result<ComparatorTraceId> {
        self.next_ulid().map(ComparatorTraceId::from_ulid)
    }

    /// 生成复核动作 id。
    ///
    /// # Errors
    ///
    /// 同 [`UlidFactory::next_ulid`]。
    pub fn next_review_action_id(&self) -> Result<ReviewActionId> {
        self.next_ulid().map(ReviewActionId::from_ulid)
    }

    /// 当前毫秒（钳制到非负）。
    fn current_ms(&self) -> u64 {
        let millis = self.clock.now().timestamp_millis();
        u64::try_from(millis).unwrap_or(0)
    }

    /// 持久化最近毫秒与最近 ULID（仅在变化时写）。
    fn persist(&self, ulid: Ulid) -> Result<()> {
        let Some(path) = &self.persist_path else {
            return Ok(());
        };
        let mut state = self
            .state
            .lock()
            .map_err(|_| PlatformError::Internal("ULID 锁中毒".to_owned()))?;
        let ms = ulid.timestamp_ms();
        if state.persisted_ms == Some(ms) && state.persisted_ulid == Some(ulid) {
            return Ok(());
        }
        let payload = serde_json::to_vec(&PersistedState {
            last_ms: ms,
            last_ulid: Some(ulid.to_string()),
        })
        .map_err(|error| PlatformError::Serialize(error.to_string()))?;
        let tmp = path.with_extension("state.tmp");
        {
            let mut file = std::fs::File::create(&tmp)
                .map_err(|source| PlatformError::io(tmp.clone(), source))?;
            file.write_all(&payload)
                .map_err(|source| PlatformError::io(tmp.clone(), source))?;
            file.sync_all()
                .map_err(|source| PlatformError::io(tmp.clone(), source))?;
        }
        std::fs::rename(&tmp, path).map_err(|source| PlatformError::io(path.clone(), source))?;
        state.persisted_ms = Some(ms);
        state.persisted_ulid = Some(ulid);
        Ok(())
    }
}

impl FactoryState {
    /// 生成下一个随机部分（80 位）。
    fn next_random(&mut self) -> u128 {
        let high = u128::from(next_u64(&mut self.rng));
        let low = u128::from(next_u64(&mut self.rng));
        ((high << 64) | low) & RANDOM_MASK
    }
}

/// xorshift64* 一步。
fn next_u64(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    *state = x;
    x.wrapping_mul(0x2545_F491_4F6C_DD1D)
}

/// 随机种子：时间纳秒 ^ 进程号 ^ 一个常数。
fn seed_rng() -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0x9E37_79B9_7F4A_7C15, |d| d.as_nanos() as u64);
    let pid = u64::from(std::process::id());
    nanos ^ pid.rotate_left(17) ^ 0x9E37_79B9_7F4A_7C15
}

/// 读取持久化状态；文件不存在视为空状态。
fn read_state(path: &Path) -> Result<(u64, Option<Ulid>)> {
    match std::fs::read(path) {
        Ok(bytes) => {
            let state: PersistedState = serde_json::from_slice(&bytes).map_err(|error| {
                PlatformError::io(
                    path.to_path_buf(),
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("ULID 状态文件损坏：{error}"),
                    ),
                )
            })?;
            let last = match state.last_ulid {
                Some(text) => Some(Ulid::parse(&text).map_err(|error| {
                    PlatformError::io(
                        path.to_path_buf(),
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            format!("ULID 状态文件中的 last_ulid 非法：{error}"),
                        ),
                    )
                })?),
                None => None,
            };
            Ok((state.last_ms, last))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok((0, None)),
        Err(source) => Err(PlatformError::io(path.to_path_buf(), source)),
    }
}

/// 进程级单例工厂（生产使用；惰性初始化）。
///
/// # Panics
///
/// 仅在工厂初始化失败时（状态文件损坏）panic——生产启动期应优先调用
/// [`UlidFactory::with_persistence`] 并显式处理错误；此函数供不关心持久化的
/// 轻量路径（如 UI 会话 id）使用。
#[must_use]
pub fn global() -> &'static UlidFactory {
    static GLOBAL: OnceLock<UlidFactory> = OnceLock::new();
    GLOBAL.get_or_init(|| UlidFactory::new(Arc::new(SystemClock)))
}

/// 状态文件默认名（供上层拼接数据根）。
#[must_use]
pub fn state_file_name() -> &'static str {
    ULID_STATE_FILE
}

/// ULID 位宽常量（诊断包与测试使用）。
#[must_use]
pub const fn bit_widths() -> (u32, u32) {
    (TIMESTAMP_BITS, RANDOM_BITS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FixedClock;
    use chrono::Duration;

    fn fixed_at(text: &str) -> Arc<FixedClock> {
        Arc::new(FixedClock::from_rfc3339(text).unwrap())
    }

    #[test]
    fn same_millisecond_is_strictly_increasing() {
        let clock = fixed_at("2026-11-02T00:00:00.000Z");
        let factory = UlidFactory::new(clock);
        let mut previous = factory.next_ulid().unwrap();
        for _ in 0..10_000 {
            let next = factory.next_ulid().unwrap();
            assert!(next > previous, "ULID 必须严格递增");
            previous = next;
        }
        assert_eq!(previous.timestamp_ms(), 1_793_577_600_000);
    }

    #[test]
    fn clock_rewind_does_not_go_backwards() {
        let clock = fixed_at("2026-11-02T12:00:00Z");
        let factory = UlidFactory::new(clock.clone());
        let first = factory.next_ulid().unwrap();
        clock.rewind(Duration::seconds(60)).unwrap();
        let second = factory.next_ulid().unwrap();
        assert!(second > first);
        assert!(second.timestamp_ms() >= first.timestamp_ms());
    }

    #[test]
    fn persistence_prevents_restart_rollback() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("ulid.state");
        let clock = fixed_at("2026-11-02T12:00:00Z");
        let first = {
            let factory = UlidFactory::with_persistence(clock.clone(), &path).expect("factory");
            factory.next_ulid().unwrap()
        };
        // 重启且时钟回拨 60 秒：新实例仍不得倒退。
        clock.rewind(Duration::seconds(60)).unwrap();
        let factory = UlidFactory::with_persistence(clock, &path).expect("factory2");
        let second = factory.next_ulid().unwrap();
        assert!(second > first);
    }

    #[test]
    fn corrupted_state_file_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("ulid.state");
        std::fs::write(&path, b"{not json").unwrap();
        let clock = fixed_at("2026-11-02T00:00:00Z");
        assert!(UlidFactory::with_persistence(clock, &path).is_err());
    }

    #[test]
    fn typed_ids_roundtrip() {
        let clock = fixed_at("2026-11-02T00:00:00Z");
        let factory = UlidFactory::new(clock);
        let task = factory.next_task_id().unwrap();
        assert_eq!(TaskId::parse(&task.to_string()).unwrap(), task);
        let consent = factory.next_consent_id().unwrap();
        assert!(consent.as_ulid() < factory.next_ulid().unwrap());
    }

    #[test]
    fn global_factory_is_singleton() {
        assert!(std::ptr::eq(global(), global()));
        assert_eq!(bit_widths(), (48, 80));
    }
}
