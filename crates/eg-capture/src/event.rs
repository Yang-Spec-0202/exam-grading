//! 导入事件与来源指纹（`SPEC-capture.md` §2「导入事件」、§5 日志脱敏硬约束）。
//!
//! 事件经端口送出，UI 侧按事件循环投递；本 crate 不依赖 GUI。
//! **脱敏**：事件只携带 [`SourceFingerprint`]（路径哈希前 12 位 + 扩展名），
//! 绝不携带文件路径原文、姓名或学号。

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::SyncSender;

use eg_domain::{ContentHash, Ulid};
use serde::Serialize;

use crate::report::ImportKind;

/// 来源指纹：路径哈希前 12 位 + 扩展名（不可反推路径）。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct SourceFingerprint {
    /// 路径（或测试标签）的 BLAKE3 十六进制前 12 位。
    pub path_hash: String,
    /// 小写扩展名（无点；无扩展名为空串）。
    pub extension: String,
}

impl SourceFingerprint {
    /// 由完整路径构造（库调用方传入绝对路径；本 crate 不保存原文）。
    pub fn of_path(path: &str) -> Self {
        let extension = path
            .rsplit_once('.')
            .map(|(_, ext)| ext.to_ascii_lowercase())
            .unwrap_or_default();
        Self {
            path_hash: path_hash(path),
            extension,
        }
    }

    /// 由任意标签构造（测试/内存输入用）。
    pub fn of_label(label: &str) -> Self {
        Self {
            path_hash: path_hash(label),
            extension: String::new(),
        }
    }

    /// 覆盖扩展名（用于内存输入按文件名推断）。
    pub fn with_extension(mut self, file_name: &str) -> Self {
        self.extension = file_name
            .rsplit_once('.')
            .map(|(_, ext)| ext.to_ascii_lowercase())
            .unwrap_or_default();
        self
    }
}

fn path_hash(path: &str) -> String {
    let hex = ContentHash::of(path.as_bytes()).to_hex();
    hex[..12].to_owned()
}

/// 导入事件（`SPEC-capture.md` §2：批次/文件/页面/进度/结束）。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum ImportEvent {
    /// 批次开始（含命名提示是否出现、选项哈希）。
    BatchStarted {
        /// 批次 id。
        batch_id: Ulid,
        /// 导入种类。
        kind: ImportKind,
        /// 文件总数。
        total_files: u32,
        /// 是否显示「建议按 学生名/学号 命名」提示（仅一次）。
        naming_reminder: bool,
        /// 选项哈希（审计/重放）。
        options_hash: String,
    },
    /// 文件开始处理。
    FileStarted {
        /// 批次 id。
        batch_id: Ulid,
        /// 来源指纹。
        fingerprint: SourceFingerprint,
        /// 字节数。
        byte_len: u64,
    },
    /// 单页解码完成。
    PageDecoded {
        /// 批次 id。
        batch_id: Ulid,
        /// 来源指纹。
        fingerprint: SourceFingerprint,
        /// 页号（1 起）。
        page_no: u32,
        /// 页宽（原图像素）。
        width: u32,
        /// 页高（原图像素）。
        height: u32,
    },
    /// 单页预处理完成（含判定标签与派生图数量；被拒页另有 `PageRejected`）。
    PagePreprocessed {
        /// 批次 id。
        batch_id: Ulid,
        /// 来源指纹。
        fingerprint: SourceFingerprint,
        /// 页号（1 起）。
        page_no: u32,
        /// 判定标签（`good` / `acceptable`）。
        verdict: &'static str,
        /// 派生图数量。
        derived_count: u8,
    },
    /// 单页被质量门禁拒收（不进识别）。
    PageRejected {
        /// 批次 id。
        batch_id: Ulid,
        /// 来源指纹。
        fingerprint: SourceFingerprint,
        /// 页号（1 起）。
        page_no: u32,
        /// 原因码列表（稳定码，非中文）。
        reasons: Vec<&'static str>,
    },
    /// 单页失败（解码/渲染/存储）。
    PageFailed {
        /// 批次 id。
        batch_id: Ulid,
        /// 来源指纹。
        fingerprint: SourceFingerprint,
        /// 页号（1 起）。
        page_no: u32,
        /// 错误码。
        code: &'static str,
    },
    /// 文件完成。
    FileFinished {
        /// 批次 id。
        batch_id: Ulid,
        /// 来源指纹。
        fingerprint: SourceFingerprint,
        /// 成功入库页数。
        page_count: u32,
        /// 新建提交 id（参考答案为 `None`）。
        submission_id: Option<Ulid>,
        /// 是否有页被拒。
        has_rejected: bool,
    },
    /// 文件级失败（不影响批次其余文件）。
    FileFailed {
        /// 批次 id。
        batch_id: Ulid,
        /// 来源指纹。
        fingerprint: SourceFingerprint,
        /// 错误码。
        code: &'static str,
        /// 教师可读消息。
        message: String,
    },
    /// 进度（已完成 / 总数）。
    Progress {
        /// 批次 id。
        batch_id: Ulid,
        /// 已完成文件数。
        completed: u32,
        /// 文件总数。
        total: u32,
    },
    /// 批次结束。
    BatchFinished {
        /// 批次 id。
        batch_id: Ulid,
        /// 是否部分成功。
        partial_success: bool,
        /// 是否被取消。
        cancelled: bool,
    },
}

impl ImportEvent {
    /// 所属批次 id（所有事件都带）。
    pub const fn batch_id(&self) -> Ulid {
        match self {
            Self::BatchStarted { batch_id, .. }
            | Self::FileStarted { batch_id, .. }
            | Self::PageDecoded { batch_id, .. }
            | Self::PagePreprocessed { batch_id, .. }
            | Self::PageRejected { batch_id, .. }
            | Self::PageFailed { batch_id, .. }
            | Self::FileFinished { batch_id, .. }
            | Self::FileFailed { batch_id, .. }
            | Self::Progress { batch_id, .. }
            | Self::BatchFinished { batch_id, .. } => *batch_id,
        }
    }

    /// 来源指纹（批次/进度/结束事件为空）。
    pub const fn fingerprint(&self) -> Option<&SourceFingerprint> {
        match self {
            Self::FileStarted { fingerprint, .. }
            | Self::PageDecoded { fingerprint, .. }
            | Self::PagePreprocessed { fingerprint, .. }
            | Self::PageRejected { fingerprint, .. }
            | Self::PageFailed { fingerprint, .. }
            | Self::FileFinished { fingerprint, .. }
            | Self::FileFailed { fingerprint, .. } => Some(fingerprint),
            Self::BatchStarted { .. } | Self::Progress { .. } | Self::BatchFinished { .. } => None,
        }
    }
}

/// 事件接收端口（`app` 侧按事件循环投递；测试用内存实现）。
pub trait ImportEventSink: Send + Sync {
    /// 送出一个事件；实现方不得阻塞导入流水线（有界通道可反压，但不得丢弃）。
    fn emit(&self, event: ImportEvent);
}

/// 丢弃全部事件的实现（无 UI 的批处理/基准用）。
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopEventSink;

impl ImportEventSink for NoopEventSink {
    fn emit(&self, _event: ImportEvent) {}
}

/// 内存事件收集器（测试与诊断用；按到达顺序保存）。
#[derive(Debug, Default)]
pub struct MemoryEventSink {
    events: Mutex<Vec<ImportEvent>>,
}

impl MemoryEventSink {
    /// 新建空收集器。
    pub fn new() -> Self {
        Self::default()
    }

    /// 已收集事件快照。
    pub fn events(&self) -> Vec<ImportEvent> {
        self.events
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }

    /// 已收集事件数。
    pub fn len(&self) -> usize {
        self.events.lock().map(|guard| guard.len()).unwrap_or(0)
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl ImportEventSink for MemoryEventSink {
    fn emit(&self, event: ImportEvent) {
        if let Ok(mut guard) = self.events.lock() {
            guard.push(event);
        }
    }
}

/// 有界通道事件端口：`send` 反压而不是丢弃（`SPEC-capture.md` §2「不丢事件」）。
#[derive(Debug)]
pub struct ChannelEventSink {
    sender: SyncSender<ImportEvent>,
    sent: AtomicU64,
}

impl ChannelEventSink {
    /// 用调用方创建的有界通道构造。
    pub fn new(sender: SyncSender<ImportEvent>) -> Self {
        Self {
            sender,
            sent: AtomicU64::new(0),
        }
    }

    /// 已成功送出的事件数（接收端断开后不再增长）。
    pub fn sent(&self) -> u64 {
        self.sent.load(Ordering::Relaxed)
    }
}

impl ImportEventSink for ChannelEventSink {
    fn emit(&self, event: ImportEvent) {
        if self.sender.send(event).is_ok() {
            self.sent.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn fingerprint_hashes_path_without_leaking_it() {
        let fp = SourceFingerprint::of_path("/Users/teacher/作业/张三 期中.jpg");
        assert_eq!(fp.path_hash.len(), 12);
        assert_eq!(fp.extension, "jpg");
        assert!(!fp.path_hash.contains("张三"));
        assert!(!fp.path_hash.contains("teacher"));
        // 同一路径稳定、不同路径不同。
        assert_eq!(
            fp,
            SourceFingerprint::of_path("/Users/teacher/作业/张三 期中.jpg")
        );
        assert_ne!(
            fp.path_hash,
            SourceFingerprint::of_path("/other/x.jpg").path_hash
        );
    }

    #[test]
    fn memory_sink_collects_in_order() {
        let sink = MemoryEventSink::new();
        let batch = Ulid::from_u128(7);
        sink.emit(ImportEvent::BatchStarted {
            batch_id: batch,
            kind: ImportKind::StudentWork,
            total_files: 2,
            naming_reminder: true,
            options_hash: "h".to_owned(),
        });
        sink.emit(ImportEvent::Progress {
            batch_id: batch,
            completed: 1,
            total: 2,
        });
        assert_eq!(sink.len(), 2);
        assert_eq!(sink.events()[0].batch_id(), batch);
        assert!(sink.events()[0].fingerprint().is_none());
    }

    #[test]
    fn channel_sink_counts_sent_events() {
        let (tx, rx) = std::sync::mpsc::sync_channel(4);
        let sink = ChannelEventSink::new(tx);
        sink.emit(ImportEvent::BatchFinished {
            batch_id: Ulid::from_u128(1),
            partial_success: false,
            cancelled: false,
        });
        assert_eq!(sink.sent(), 1);
        assert_eq!(rx.try_recv().unwrap().batch_id(), Ulid::from_u128(1));
    }
}
