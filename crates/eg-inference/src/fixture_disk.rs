//! 从仓库级 `fixtures/` 目录加载夹具（跨进程/跨 crate 的确定性端到端）。
//!
//! 背景：[`FixtureStore`] / [`FixtureBackend`] 只认「精确字节键」——键里含
//! 夹具录制时的模型 id/版本。跨进程 E2E（`egctl`/`recognize`/`grade`）用的注册表
//! 模型 id 与录制时不同（录制用 `fixture-ocr`/`fixture-judge`），或输入是 capture
//! 派生图（字节不同），精确键会 `FixtureMiss`。本模块把仓库夹具的 **索引**
//! （`fixtures/<role>/index.json`）也读进来，提供三级匹配：
//!
//! 1. **精确字节键**（最高优先级）：`fixture_key(model_id, version, schema, prompt, input)`；
//! 2. **组合键**：`(角色, prompt 的 sha256, 输入资产的 sha256)`，
//!    可再限定模型 id/版本（不限定即忽略模型身份——这正是跨注册表 id 回放的关键）；
//! 3. **语义标识**：OCR `(学生号或文件 stem, 页码)`；judge `(学生号, 题号)`。
//!
//! 匹配规则（fail-closed）：
//! - 精确键未命中且给了组合键 → 继续按组合键匹配（仍不算「猜」：提示词与输入字节
//!   sha256 都相等才命中）；组合键也未命中 → [`InferenceError::FixtureMiss`]；
//! - 任何一级匹配到多条视为夹具集损坏 → [`InferenceError::Internal`]（歧义不猜）；
//! - `negative/` 样例**只**装载用于拒绝路径测试，绝不进入回放索引。
//!
//! 装载期完整性校验（任何不一致即拒绝加载，避免「回放错页」）：
//! - 每个记录文件、提示词文件、输入资产文件都按索引给出的 sha256 复核；
//! - 用仓库内 [`fixture_key`] 从「提示词字节 + 输入字节 + 索引里的模型身份」
//!   **重算**键，必须等于记录里的 `key` 与索引里的 `fixture_key`；
//! - `output_raw` 的 sha256 必须等于索引 `output_sha256`，且必须是 JSON 对象。
//!
//! 实测夹具：`fixtures/{ocr,judge}/index.json`（由 `fixtures/generator/keygen` 生成，
//! 见 `docs/dev/research/inference.md` §5.3）。

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use eg_domain::Sha256Hex;
use serde::Deserialize;

use crate::error::InferenceError;
use crate::fixture::{
    FixtureBackend, FixtureEntry, FixtureReplay, FixtureStore, fixture_key, read_entry_file,
};

/// 夹具索引 schema 版本（`fixtures/<role>/index.json`）。
pub const FIXTURE_INDEX_SCHEMA_VERSION: u32 = 1;

/// 夹具角色（对应索引 `kind` 与目录名 `ocr`/`judge`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FixtureRole {
    /// 页级 OCR 识别。
    Ocr,
    /// 单题判定。
    Judge,
}

impl FixtureRole {
    /// 稳定目录名/索引 `kind` 文本。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ocr => "ocr",
            Self::Judge => "judge",
        }
    }

    /// 解析索引 `kind`。
    ///
    /// # Errors
    ///
    /// 未知 kind 返回 [`InferenceError::Internal`]（夹具集损坏）。
    pub fn parse(text: &str) -> Result<Self, InferenceError> {
        match text.trim() {
            "ocr" => Ok(Self::Ocr),
            "judge" => Ok(Self::Judge),
            other => Err(disk(format!(
                "夹具索引 kind={other:?} 不受支持（只认 ocr/judge）"
            ))),
        }
    }
}

/// OCR 页级语义标识（真实 capture 派生图字节与夹具输入不同时使用）。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PageIdentity {
    /// 学生号（`SO2026-00x`）；相机文件名等未决页为 `None`。
    pub student_no: Option<String>,
    /// capture 原样保留的文件 stem（`张三_p1` 的 `张三`）。
    pub submission_stem: Option<String>,
    /// 页码（1 起）。
    pub page_no: u32,
}

impl PageIdentity {
    /// 按学生号 + 页码定位。
    #[must_use]
    pub fn by_student(student_no: impl Into<String>, page_no: u32) -> Self {
        Self {
            student_no: Some(student_no.into()),
            submission_stem: None,
            page_no,
        }
    }

    /// 按文件 stem + 页码定位（相机文件名等未决页）。
    #[must_use]
    pub fn by_stem(submission_stem: impl Into<String>, page_no: u32) -> Self {
        Self {
            student_no: None,
            submission_stem: Some(submission_stem.into()),
            page_no,
        }
    }
}

/// judge 题级语义标识。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct QuestionIdentity {
    /// 学生号。
    pub student_no: String,
    /// 题号（1 起，与 `paper.json` 的 ordinal 一致）。
    pub question_ordinal: u32,
}

impl QuestionIdentity {
    /// 构造题级标识。
    #[must_use]
    pub fn new(student_no: impl Into<String>, question_ordinal: u32) -> Self {
        Self {
            student_no: student_no.into(),
            question_ordinal,
        }
    }
}

/// 一条已装载且通过完整性校验的正向夹具记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedFixture {
    /// 夹具记录（键 + 录制输出）。
    pub entry: FixtureEntry,
    /// 角色。
    pub role: FixtureRole,
    /// 提示词字节的 sha256（组合键成员）。
    pub prompt_sha256: Sha256Hex,
    /// 输入资产字节的 sha256（组合键成员；OCR 为识别输入图，judge 为输入 JSON）。
    pub asset_sha256: Sha256Hex,
    /// OCR 页标识（judge 记录为 `None`）。
    pub page: Option<PageIdentity>,
    /// judge 题标识（OCR 记录为 `None`）。
    pub question: Option<QuestionIdentity>,
    /// 索引里的用例分类（judge 的 `normal`/`conflict` 等；OCR 通常为 `None`）。
    pub case: Option<String>,
    /// 记录文件路径（诊断/审计用）。
    pub source: PathBuf,
    /// 提示词文件路径（接线方按需读取；`LoadedFixture` 本身不缓存提示词字节）。
    pub prompt_source: PathBuf,
    /// 输入资产路径（OCR 识别输入图 / judge 规范化输入 JSON）。
    pub asset_source: PathBuf,
}

/// 一条 `negative/` 样例：只用于「必须被拒绝」的断言，不参与回放。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedNegative {
    /// 索引里的样例 id。
    pub id: String,
    /// 夹具记录（其输出应为畸形/非法样例）。
    pub entry: FixtureEntry,
    /// 角色。
    pub role: FixtureRole,
    /// 索引给出的说明（为什么必须拒绝）。
    pub note: String,
    /// 记录文件路径（诊断/审计用）。
    pub source: PathBuf,
}

/// 索引里声明的夹具后端身份（录制时的模型 id/版本/提示词 Schema）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixtureBackendIds {
    /// 录制时的模型 id（如 `fixture-ocr`）。
    pub model_id: String,
    /// 录制时的模型版本。
    pub model_version: String,
    /// 录制时的提示词 Schema 版本。
    pub prompt_schema_version: String,
}

/// 一次匹配请求（按需组合；未设置的维度不参与过滤）。
///
/// 优先级见模块文档：精确键 → 组合键 → 语义标识。
#[derive(Debug, Clone, Default)]
pub struct FixtureLookup {
    exact_key: Option<Sha256Hex>,
    role: Option<FixtureRole>,
    model_id: Option<String>,
    model_version: Option<String>,
    prompt_sha256: Option<Sha256Hex>,
    asset_sha256: Option<Sha256Hex>,
    page: Option<PageIdentity>,
    question: Option<QuestionIdentity>,
}

impl FixtureLookup {
    /// 空匹配请求（直接 `resolve` 必然 `FixtureMiss`；用于逐项构造）。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 只按精确字节键匹配（最高优先级）。
    #[must_use]
    pub fn exact(mut self, key: Sha256Hex) -> Self {
        self.exact_key = Some(key);
        self
    }

    /// 限定角色（不设置则组合键同时查 OCR/judge）。
    #[must_use]
    pub fn role(mut self, role: FixtureRole) -> Self {
        self.role = Some(role);
        self
    }

    /// 限定模型身份（组合键命中后复核，不相等视为该级未命中）。
    #[must_use]
    pub fn model(mut self, model_id: impl Into<String>, model_version: impl Into<String>) -> Self {
        self.model_id = Some(model_id.into());
        self.model_version = Some(model_version.into());
        self
    }

    /// 组合键：提示词 sha256 + 输入资产 sha256。
    #[must_use]
    pub fn hashes(mut self, prompt_sha256: Sha256Hex, asset_sha256: Sha256Hex) -> Self {
        self.prompt_sha256 = Some(prompt_sha256);
        self.asset_sha256 = Some(asset_sha256);
        self
    }

    /// 语义标识：OCR 页。
    #[must_use]
    pub fn page(mut self, page: PageIdentity) -> Self {
        self.page = Some(page);
        self
    }

    /// 语义标识：judge 题。
    #[must_use]
    pub fn question(mut self, question: QuestionIdentity) -> Self {
        self.question = Some(question);
        self
    }

    /// 未命中时的诊断文本（不含图像字节等隐私内容）。
    fn describe(&self) -> String {
        let hex = |value: Option<Sha256Hex>| value.map(Sha256Hex::to_hex);
        format!(
            "exact={:?} role={:?} model={:?}/{:?} prompt={:?} asset={:?} page={:?} question={:?}",
            self.exact_key.map(Sha256Hex::to_hex),
            self.role,
            self.model_id,
            self.model_version,
            hex(self.prompt_sha256),
            hex(self.asset_sha256),
            self.page,
            self.question
        )
    }
}

/// 仓库夹具目录（`fixtures/ocr` + `fixtures/judge`）的只读目录与索引。
///
/// 装载即校验（见模块文档）；`Clone` 仅复制索引与记录（输出文本共享不可变数据）。
#[derive(Debug, Clone, Default)]
pub struct FixtureCatalog {
    records: Vec<LoadedFixture>,
    negatives: Vec<LoadedNegative>,
    by_exact: BTreeMap<Sha256Hex, usize>,
    by_hashes: BTreeMap<(FixtureRole, Sha256Hex, Sha256Hex), usize>,
    by_page: BTreeMap<(FixtureRole, String, u32), usize>,
    by_stem: BTreeMap<(FixtureRole, String, u32), usize>,
    by_question: BTreeMap<(FixtureRole, String, u32), usize>,
    backends: BTreeMap<FixtureRole, FixtureBackendIds>,
}

impl FixtureCatalog {
    /// 空目录（`load`/`load_role` 之前的状态）。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 从 `fixtures/` 根加载存在的 `ocr/` 与 `judge/`。
    ///
    /// # Errors
    ///
    /// 两个角色都不存在、或任一索引/记录/资产校验失败时返回错误（fail-closed）。
    pub fn load(fixtures_root: &Path) -> Result<Self, InferenceError> {
        let mut catalog = Self::new();
        let mut loaded_roles = 0_u32;
        for role in [FixtureRole::Ocr, FixtureRole::Judge] {
            let dir = fixtures_root.join(role.as_str());
            if dir.join("index.json").is_file() {
                catalog.load_role_dir(Some(role), &dir)?;
                loaded_roles += 1;
            }
        }
        if loaded_roles == 0 {
            return Err(disk(format!(
                "{} 下没有 ocr/judge 夹具索引（期望 <root>/ocr/index.json 或 <root>/judge/index.json）",
                fixtures_root.display()
            )));
        }
        Ok(catalog)
    }

    /// 从单个角色目录加载（如 `fixtures/ocr`）；路径相对 `fixtures/` 根解析。
    ///
    /// # Errors
    ///
    /// 索引缺失/损坏、记录与资产 sha256 或重算键不一致时返回错误。
    pub fn load_role(role_dir: &Path) -> Result<Self, InferenceError> {
        let mut catalog = Self::new();
        catalog.load_role_dir(None, role_dir)?;
        Ok(catalog)
    }

    fn load_role_dir(
        &mut self,
        expected_role: Option<FixtureRole>,
        role_dir: &Path,
    ) -> Result<(), InferenceError> {
        let index_path = role_dir.join("index.json");
        let text = std::fs::read_to_string(&index_path).map_err(|error| {
            disk(format!(
                "无法读取夹具索引 {}：{error}",
                index_path.display()
            ))
        })?;
        let raw: RawIndex = serde_json::from_str(&text).map_err(|error| {
            disk(format!(
                "夹具索引 {} 不是合法 JSON：{error}",
                index_path.display()
            ))
        })?;
        if raw.schema_version != FIXTURE_INDEX_SCHEMA_VERSION {
            return Err(disk(format!(
                "夹具索引 {} 的 schema_version={} 不受支持",
                index_path.display(),
                raw.schema_version
            )));
        }
        let role = FixtureRole::parse(&raw.kind)?;
        if let Some(expected) = expected_role
            && expected != role
        {
            return Err(disk(format!(
                "夹具目录 {} 的 kind={} 与会话中的角色 {} 不一致",
                role_dir.display(),
                raw.kind,
                expected.as_str()
            )));
        }
        if self.backends.contains_key(&role) {
            return Err(disk(format!("夹具角色 {} 被重复加载", role.as_str())));
        }
        if raw.backend.model_id.trim().is_empty()
            || raw.backend.model_version.trim().is_empty()
            || raw.backend.prompt_schema_version.trim().is_empty()
        {
            return Err(disk(format!(
                "夹具索引 {} 的 backend 身份不完整",
                index_path.display()
            )));
        }
        // 记录里的路径是相对 `fixtures/` 根的（`ocr/pages/x.json`、`judge/inputs/y.json`）。
        let base = role_dir
            .parent()
            .ok_or_else(|| disk(format!("夹具目录 {} 没有上级根目录", role_dir.display())))?;
        let ids = FixtureBackendIds {
            model_id: raw.backend.model_id.clone(),
            model_version: raw.backend.model_version.clone(),
            prompt_schema_version: raw.backend.prompt_schema_version.clone(),
        };
        self.backends.insert(role, ids.clone());
        for record in &raw.records {
            let loaded = load_record(role, &ids, base, record)?;
            self.index_record(loaded)?;
        }
        for negative in &raw.negative {
            let path = resolve_relative(base, &negative.file, "negative 记录")?;
            let entry = read_entry_file(&path)?;
            self.negatives.push(LoadedNegative {
                id: negative.id.clone(),
                entry,
                role,
                note: negative.note.clone(),
                source: path,
            });
        }
        Ok(())
    }

    fn index_record(&mut self, loaded: LoadedFixture) -> Result<(), InferenceError> {
        let index = self.records.len();
        let role = loaded.role;
        let exact = loaded.entry.key;
        if self.by_exact.contains_key(&exact) {
            return Err(disk(format!(
                "夹具精确键重复：{}（{}）",
                exact.to_hex(),
                loaded.source.display()
            )));
        }
        let hashes = (role, loaded.prompt_sha256, loaded.asset_sha256);
        if self.by_hashes.contains_key(&hashes) {
            return Err(disk(format!(
                "夹具组合键重复（{} 角色同提示词+同输入）：{}",
                role.as_str(),
                loaded.source.display()
            )));
        }
        if let Some(page) = &loaded.page {
            let stem_key = (
                role,
                page.submission_stem.clone().unwrap_or_default(),
                page.page_no,
            );
            if self.by_stem.contains_key(&stem_key) {
                return Err(disk(format!(
                    "夹具页标识重复（{} stem={:?} 第 {} 页）：{}",
                    role.as_str(),
                    stem_key.1,
                    page.page_no,
                    loaded.source.display()
                )));
            }
            self.by_stem.insert(stem_key, index);
            if let Some(student_no) = &page.student_no {
                let student_key = (role, student_no.clone(), page.page_no);
                if self.by_page.contains_key(&student_key) {
                    return Err(disk(format!(
                        "夹具页标识重复（{} 学生={student_no} 第 {} 页）：{}",
                        role.as_str(),
                        page.page_no,
                        loaded.source.display()
                    )));
                }
                self.by_page.insert(student_key, index);
            }
        }
        if let Some(question) = &loaded.question {
            let key = (role, question.student_no.clone(), question.question_ordinal);
            if self.by_question.contains_key(&key) {
                return Err(disk(format!(
                    "夹具题标识重复（{} 学生={} 第 {} 题）：{}",
                    role.as_str(),
                    question.student_no,
                    question.question_ordinal,
                    loaded.source.display()
                )));
            }
            self.by_question.insert(key, index);
        }
        self.by_exact.insert(exact, index);
        self.by_hashes.insert(hashes, index);
        self.records.push(loaded);
        Ok(())
    }

    /// 按优先级解析一次匹配请求；未命中返回 [`InferenceError::FixtureMiss`]。
    ///
    /// # Errors
    ///
    /// 未命中、或某一级出现歧义（夹具集损坏）时返回错误。
    pub fn resolve(&self, lookup: &FixtureLookup) -> Result<FixtureReplay, InferenceError> {
        if let Some(key) = lookup.exact_key
            && let Some(replay) = self.exact_opt(key)
        {
            return Ok(replay);
        }
        if let (Some(prompt), Some(asset)) = (lookup.prompt_sha256, lookup.asset_sha256)
            && let Some(replay) = self.combination_opt(
                lookup.role,
                prompt,
                asset,
                lookup.model_id.as_deref(),
                lookup.model_version.as_deref(),
            )?
        {
            return Ok(replay);
        }
        if let Some(page) = &lookup.page
            && let Some(replay) = self.page_opt(lookup.role, page)?
        {
            return Ok(replay);
        }
        if let Some(question) = &lookup.question
            && let Some(replay) = self.question_opt(lookup.role, question)
        {
            return Ok(replay);
        }
        Err(InferenceError::FixtureMiss {
            key: lookup.describe(),
        })
    }

    /// 只按精确字节键回放。
    ///
    /// # Errors
    ///
    /// 键不存在时返回 [`InferenceError::FixtureMiss`]。
    pub fn exact(&self, key: Sha256Hex) -> Result<FixtureReplay, InferenceError> {
        self.exact_opt(key)
            .ok_or_else(|| InferenceError::FixtureMiss { key: key.to_hex() })
    }

    /// 只按组合键回放（角色 + prompt sha256 + 资产 sha256）。
    ///
    /// # Errors
    ///
    /// 未命中时返回 [`InferenceError::FixtureMiss`]。
    pub fn by_hashes(
        &self,
        role: FixtureRole,
        prompt_sha256: Sha256Hex,
        asset_sha256: Sha256Hex,
    ) -> Result<FixtureReplay, InferenceError> {
        self.combination_opt(Some(role), prompt_sha256, asset_sha256, None, None)?
            .ok_or_else(|| InferenceError::FixtureMiss {
                key: format!(
                    "role={} prompt={} asset={}",
                    role.as_str(),
                    prompt_sha256.to_hex(),
                    asset_sha256.to_hex()
                ),
            })
    }

    /// 只按 OCR 页标识回放（语义映射模式）。
    ///
    /// # Errors
    ///
    /// 未命中或标识歧义时返回错误。
    pub fn by_page(&self, page: &PageIdentity) -> Result<FixtureReplay, InferenceError> {
        self.page_opt(Some(FixtureRole::Ocr), page)?
            .ok_or_else(|| InferenceError::FixtureMiss {
                key: format!("page={page:?}"),
            })
    }

    /// 只按 judge 题标识回放（语义映射模式）。
    ///
    /// # Errors
    ///
    /// 未命中时返回 [`InferenceError::FixtureMiss`]。
    pub fn by_question(
        &self,
        question: &QuestionIdentity,
    ) -> Result<FixtureReplay, InferenceError> {
        self.question_opt(Some(FixtureRole::Judge), question)
            .ok_or_else(|| InferenceError::FixtureMiss {
                key: format!(
                    "学生={} 第 {} 题（judge）",
                    question.student_no, question.question_ordinal
                ),
            })
    }

    fn exact_opt(&self, key: Sha256Hex) -> Option<FixtureReplay> {
        self.by_exact
            .get(&key)
            .map(|index| replay_of(&self.records[*index]))
    }

    fn combination_opt(
        &self,
        role: Option<FixtureRole>,
        prompt_sha256: Sha256Hex,
        asset_sha256: Sha256Hex,
        model_id: Option<&str>,
        model_version: Option<&str>,
    ) -> Result<Option<FixtureReplay>, InferenceError> {
        let mut hit: Option<&LoadedFixture> = None;
        for candidate_role in candidate_roles(role) {
            let key = (candidate_role, prompt_sha256, asset_sha256);
            let Some(index) = self.by_hashes.get(&key) else {
                continue;
            };
            let record = &self.records[*index];
            if let Some(expected) = model_id
                && record.entry.model_id != expected
            {
                continue;
            }
            if let Some(expected) = model_version
                && record.entry.model_version != expected
            {
                continue;
            }
            if hit.is_some() {
                return Err(disk(
                    "夹具组合键命中多条（角色+提示词+输入相同）——夹具集损坏，拒绝猜测",
                ));
            }
            hit = Some(record);
        }
        Ok(hit.map(replay_of))
    }

    fn page_opt(
        &self,
        role: Option<FixtureRole>,
        page: &PageIdentity,
    ) -> Result<Option<FixtureReplay>, InferenceError> {
        if let Some(role) = role
            && role != FixtureRole::Ocr
        {
            return Ok(None);
        }
        let role = FixtureRole::Ocr;
        let by_student = page.student_no.as_ref().and_then(|student_no| {
            self.by_page
                .get(&(role, student_no.clone(), page.page_no))
                .copied()
        });
        let by_stem = page.submission_stem.as_ref().and_then(|stem| {
            self.by_stem
                .get(&(role, stem.clone(), page.page_no))
                .copied()
        });
        match (by_student, by_stem) {
            (Some(left), Some(right)) if left != right => Err(disk(format!(
                "夹具页标识歧义（学生号与文件 stem 指向不同记录，第 {} 页）——夹具集损坏",
                page.page_no
            ))),
            (Some(index), _) | (None, Some(index)) => Ok(Some(replay_of(&self.records[index]))),
            (None, None) => Ok(None),
        }
    }

    fn question_opt(
        &self,
        role: Option<FixtureRole>,
        question: &QuestionIdentity,
    ) -> Option<FixtureReplay> {
        if let Some(role) = role
            && role != FixtureRole::Judge
        {
            return None;
        }
        self.by_question
            .get(&(
                FixtureRole::Judge,
                question.student_no.clone(),
                question.question_ordinal,
            ))
            .map(|index| replay_of(&self.records[*index]))
    }

    /// 精确键回退组合键的入口（供 [`FixtureBackend`] 跨模型 id 回放用）。
    ///
    /// # Errors
    ///
    /// 组合键命中多条（夹具集损坏）时返回错误。
    pub fn try_combination(
        &self,
        role: FixtureRole,
        prompt_sha256: Sha256Hex,
        asset_sha256: Sha256Hex,
    ) -> Result<Option<FixtureReplay>, InferenceError> {
        self.combination_opt(Some(role), prompt_sha256, asset_sha256, None, None)
    }

    /// 构造该角色的精确回放后端（并挂接本目录作为组合键回退）。
    ///
    /// 这样调用方即使注册表里的模型 id 与录制 id 不同，也能确定性回放。
    ///
    /// # Errors
    ///
    /// 该角色未加载时返回错误。
    pub fn fixture_backend(&self, role: FixtureRole) -> Result<FixtureBackend, InferenceError> {
        let ids = self.backend_ids(role).ok_or_else(|| {
            disk(format!(
                "夹具目录没有装载角色 {}，无法构造回放后端",
                role.as_str()
            ))
        })?;
        let store = self.fixture_store(role)?;
        Ok(FixtureBackend::new(
            ids.model_id.clone(),
            ids.model_version.clone(),
            ids.prompt_schema_version.clone(),
            store,
        )
        .with_catalog(Arc::new(self.clone()), role))
    }

    /// 该角色的精确键索引（供调用方自建 [`FixtureBackend`]）。
    ///
    /// # Errors
    ///
    /// 该角色未加载时返回错误。
    pub fn fixture_store(&self, role: FixtureRole) -> Result<FixtureStore, InferenceError> {
        if !self.backends.contains_key(&role) {
            return Err(disk(format!("夹具目录没有装载角色 {}", role.as_str())));
        }
        let mut store = FixtureStore::new();
        for record in self.by_role(role) {
            store.insert(record.entry.clone())?;
        }
        Ok(store)
    }

    /// 索引里声明的后端身份。
    #[must_use]
    pub fn backend_ids(&self, role: FixtureRole) -> Option<&FixtureBackendIds> {
        self.backends.get(&role)
    }

    /// 某角色的全部正向记录。
    #[must_use]
    pub fn by_role(&self, role: FixtureRole) -> Vec<&LoadedFixture> {
        self.records
            .iter()
            .filter(|record| record.role == role)
            .collect()
    }

    /// 全部正向记录。
    #[must_use]
    pub fn records(&self) -> &[LoadedFixture] {
        &self.records
    }

    /// 全部 `negative/` 样例（只用于拒绝路径，永不回放）。
    #[must_use]
    pub fn negatives(&self) -> &[LoadedNegative] {
        &self.negatives
    }

    /// 已装载角色。
    #[must_use]
    pub fn roles(&self) -> Vec<FixtureRole> {
        self.backends.keys().copied().collect()
    }

    /// 正向记录条数。
    #[must_use]
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// 是否没有任何正向记录。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
}

fn replay_of(record: &LoadedFixture) -> FixtureReplay {
    FixtureReplay {
        key: record.entry.key,
        output_raw: record.entry.output_raw.clone(),
    }
}

fn candidate_roles(role: Option<FixtureRole>) -> Vec<FixtureRole> {
    role.map_or_else(
        || vec![FixtureRole::Ocr, FixtureRole::Judge],
        |role| vec![role],
    )
}

fn load_record(
    role: FixtureRole,
    ids: &FixtureBackendIds,
    base: &Path,
    raw: &RawRecord,
) -> Result<LoadedFixture, InferenceError> {
    let id = raw.id.as_str();
    let fixture_path = resolve_relative(base, &raw.fixture, "fixture 记录")?;
    let prompt_path = resolve_relative(base, &raw.prompt_file, "提示词文件")?;
    // 资产字段按角色二选一（OCR：识别输入图；judge：规范化输入 JSON）。
    let (asset_rel, asset_sha_text) = match role {
        FixtureRole::Ocr => (
            raw.ocr_input_image.as_deref(),
            raw.ocr_input_sha256.as_deref(),
        ),
        FixtureRole::Judge => (raw.input_file.as_deref(), raw.input_sha256.as_deref()),
    };
    let asset_rel = asset_rel.ok_or_else(|| {
        disk(format!(
            "夹具记录 {id}（{}）缺少输入资产路径",
            role.as_str()
        ))
    })?;
    let asset_sha_text = asset_sha_text.ok_or_else(|| {
        disk(format!(
            "夹具记录 {id}（{}）缺少输入资产 sha256",
            role.as_str()
        ))
    })?;
    let asset_path = resolve_relative(base, asset_rel, "输入资产")?;

    let prompt_bytes = read_bytes(&prompt_path, "提示词")?;
    let prompt_text = std::str::from_utf8(&prompt_bytes).map_err(|error| {
        disk(format!(
            "夹具记录 {id} 的提示词不是 UTF-8（{}）：{error}",
            prompt_path.display()
        ))
    })?;
    let asset_bytes = read_bytes(&asset_path, "输入资产")?;

    let prompt_sha256 = Sha256Hex::of(&prompt_bytes);
    let asset_sha256 = Sha256Hex::of(&asset_bytes);
    if prompt_sha256 != parse_hash(raw.prompt_sha256.as_str(), "prompt_sha256")? {
        return Err(disk(format!(
            "夹具记录 {id} 的提示词 sha256 与索引不一致（{}）",
            prompt_path.display()
        )));
    }
    if asset_sha256 != parse_hash(asset_sha_text, "asset_sha256")? {
        return Err(disk(format!(
            "夹具记录 {id} 的输入资产 sha256 与索引不一致（{}）",
            asset_path.display()
        )));
    }

    let entry = read_entry_file(&fixture_path)?;
    if entry.model_id != ids.model_id
        || entry.model_version != ids.model_version
        || entry.prompt_schema_version != ids.prompt_schema_version
    {
        return Err(disk(format!(
            "夹具记录 {id} 的模型身份与索引 backend 不一致（{}）",
            fixture_path.display()
        )));
    }
    let computed = fixture_key(
        &ids.model_id,
        &ids.model_version,
        &ids.prompt_schema_version,
        prompt_text,
        &asset_bytes,
    );
    if computed != entry.key {
        return Err(disk(format!(
            "夹具记录 {id} 的 key 无法由「提示词+输入」重算复现（{}）——夹具已漂移",
            fixture_path.display()
        )));
    }
    if entry.key != parse_hash(raw.fixture_key.as_str(), "fixture_key")? {
        return Err(disk(format!(
            "夹具记录 {id} 的 key 与索引 fixture_key 不一致"
        )));
    }
    let output_sha256 = Sha256Hex::of(entry.output_raw.as_bytes());
    if output_sha256 != parse_hash(raw.output_sha256.as_str(), "output_sha256")? {
        return Err(disk(format!("夹具记录 {id} 的输出 sha256 与索引不一致")));
    }

    let page = match role {
        FixtureRole::Ocr => {
            let page_no = raw
                .page_no
                .ok_or_else(|| disk(format!("夹具记录 {id}（ocr）缺少 page_no")))?;
            let submission_stem = raw
                .submission_stem
                .clone()
                .ok_or_else(|| disk(format!("夹具记录 {id}（ocr）缺少 submission_stem")))?;
            Some(PageIdentity {
                student_no: raw.student_no.clone(),
                submission_stem: Some(submission_stem),
                page_no,
            })
        }
        FixtureRole::Judge => None,
    };
    let question = match role {
        FixtureRole::Ocr => None,
        FixtureRole::Judge => {
            let student_no = raw
                .student_no
                .clone()
                .ok_or_else(|| disk(format!("夹具记录 {id}（judge）缺少 student_no")))?;
            let question_ordinal = raw
                .question_ordinal
                .ok_or_else(|| disk(format!("夹具记录 {id}（judge）缺少 question_ordinal")))?;
            Some(QuestionIdentity {
                student_no,
                question_ordinal,
            })
        }
    };

    Ok(LoadedFixture {
        entry,
        role,
        prompt_sha256,
        asset_sha256,
        page,
        question,
        case: raw.case.clone(),
        source: fixture_path,
        prompt_source: prompt_path,
        asset_source: asset_path,
    })
}

fn read_bytes(path: &Path, label: &str) -> Result<Vec<u8>, InferenceError> {
    std::fs::read(path)
        .map_err(|error| disk(format!("无法读取{label} {}：{error}", path.display())))
}

fn parse_hash(text: &str, label: &str) -> Result<Sha256Hex, InferenceError> {
    Sha256Hex::from_hex(text.trim()).map_err(|_| disk(format!("{label} 不是 64 位小写十六进制")))
}

fn resolve_relative(base: &Path, relative: &str, label: &str) -> Result<PathBuf, InferenceError> {
    let path = Path::new(relative);
    let unsafe_component = path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    });
    if path.is_absolute() || unsafe_component {
        return Err(disk(format!(
            "{label} 路径必须是 fixtures 根下的相对路径，实际 {relative:?}"
        )));
    }
    Ok(base.join(path))
}

fn disk(detail: impl Into<String>) -> InferenceError {
    InferenceError::Internal {
        detail: detail.into(),
    }
}

#[derive(Debug, Deserialize)]
struct RawIndex {
    schema_version: u32,
    kind: String,
    backend: RawBackend,
    #[serde(default)]
    records: Vec<RawRecord>,
    #[serde(default)]
    negative: Vec<RawNegative>,
}

#[derive(Debug, Deserialize)]
struct RawBackend {
    model_id: String,
    model_version: String,
    prompt_schema_version: String,
}

#[derive(Debug, Deserialize)]
struct RawRecord {
    id: String,
    fixture: String,
    fixture_key: String,
    prompt_file: String,
    prompt_sha256: String,
    output_sha256: String,
    #[serde(default)]
    student_no: Option<String>,
    #[serde(default)]
    submission_stem: Option<String>,
    #[serde(default)]
    page_no: Option<u32>,
    #[serde(default)]
    question_ordinal: Option<u32>,
    #[serde(default)]
    case: Option<String>,
    #[serde(default)]
    ocr_input_image: Option<String>,
    #[serde(default)]
    ocr_input_sha256: Option<String>,
    #[serde(default)]
    input_file: Option<String>,
    #[serde(default)]
    input_sha256: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawNegative {
    id: String,
    file: String,
    #[serde(default)]
    note: String,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    const OCR_BACKEND: (&str, &str, &str) = ("fixture-ocr", "1.0", "1");
    const JUDGE_BACKEND: (&str, &str, &str) = ("fixture-judge", "1.0", "judge.outcome.v1");

    #[derive(Clone, Copy)]
    struct RecordSpec<'a> {
        id: &'a str,
        prompt: &'a str,
        asset: &'a [u8],
        output: &'a str,
        student_no: Option<&'a str>,
        submission_stem: Option<&'a str>,
        page_no: Option<u32>,
        question_ordinal: Option<u32>,
        case: Option<&'a str>,
    }

    fn record<'a>(id: &'a str, output: &'a str) -> RecordSpec<'a> {
        RecordSpec {
            id,
            prompt: "同一提示词",
            asset: b"asset-bytes",
            output,
            student_no: Some("SO2026-001"),
            submission_stem: Some("张三"),
            page_no: Some(1),
            question_ordinal: None,
            case: None,
        }
    }

    fn write_role(
        root: &Path,
        role: FixtureRole,
        backend: (&str, &str, &str),
        records: &[RecordSpec<'_>],
        negatives: &[(&str, &str, &str)],
    ) {
        let role_dir = root.join(role.as_str());
        std::fs::create_dir_all(role_dir.join("prompts")).unwrap();
        std::fs::create_dir_all(role_dir.join("inputs")).unwrap();
        std::fs::create_dir_all(role_dir.join("pages")).unwrap();
        std::fs::create_dir_all(role_dir.join("responses")).unwrap();
        std::fs::create_dir_all(role_dir.join("negative")).unwrap();

        let mut index_records = Vec::new();
        for spec in records {
            let prompt_rel = format!("{}/prompts/{}.txt", role.as_str(), spec.id);
            let asset_ext = if role == FixtureRole::Ocr {
                "png"
            } else {
                "json"
            };
            let asset_rel = format!("{}/inputs/{}.{asset_ext}", role.as_str(), spec.id);
            let fixture_rel = match role {
                FixtureRole::Ocr => format!("{}/pages/{}.json", role.as_str(), spec.id),
                FixtureRole::Judge => format!("{}/responses/{}.json", role.as_str(), spec.id),
            };
            std::fs::write(root.join(&prompt_rel), spec.prompt.as_bytes()).unwrap();
            std::fs::write(root.join(&asset_rel), spec.asset).unwrap();
            let key = fixture_key(backend.0, backend.1, backend.2, spec.prompt, spec.asset);
            let record_value = serde_json::json!({
                "schema_version": 1,
                "key": key.to_hex(),
                "model_id": backend.0,
                "model_version": backend.1,
                "prompt_schema_version": backend.2,
                "output_raw": spec.output,
            });
            std::fs::write(
                root.join(&fixture_rel),
                serde_json::to_string_pretty(&record_value).unwrap(),
            )
            .unwrap();

            let mut item = serde_json::json!({
                "id": spec.id,
                "fixture": fixture_rel,
                "fixture_key": key.to_hex(),
                "prompt_file": prompt_rel,
                "prompt_sha256": Sha256Hex::of(spec.prompt.as_bytes()).to_hex(),
                "output_sha256": Sha256Hex::of(spec.output.as_bytes()).to_hex(),
            });
            if let Some(student_no) = spec.student_no {
                item["student_no"] = serde_json::json!(student_no);
            }
            if let Some(stem) = spec.submission_stem {
                item["submission_stem"] = serde_json::json!(stem);
            }
            if let Some(page_no) = spec.page_no {
                item["page_no"] = serde_json::json!(page_no);
            }
            if let Some(ordinal) = spec.question_ordinal {
                item["question_ordinal"] = serde_json::json!(ordinal);
            }
            if let Some(case) = spec.case {
                item["case"] = serde_json::json!(case);
            }
            if role == FixtureRole::Ocr {
                item["ocr_input_image"] = serde_json::json!(asset_rel);
                item["ocr_input_sha256"] = serde_json::json!(Sha256Hex::of(spec.asset).to_hex());
            } else {
                item["input_file"] = serde_json::json!(asset_rel);
                item["input_sha256"] = serde_json::json!(Sha256Hex::of(spec.asset).to_hex());
            }
            index_records.push(item);
        }

        let mut index_negatives = Vec::new();
        for (id, note, output) in negatives {
            let rel = format!("{}/negative/{id}.json", role.as_str());
            let marker = format!("negative:{id}");
            let key = fixture_key(backend.0, backend.1, backend.2, &marker, marker.as_bytes());
            let record_value = serde_json::json!({
                "schema_version": 1,
                "key": key.to_hex(),
                "model_id": backend.0,
                "model_version": backend.1,
                "prompt_schema_version": backend.2,
                "output_raw": output,
            });
            std::fs::write(
                root.join(&rel),
                serde_json::to_string_pretty(&record_value).unwrap(),
            )
            .unwrap();
            index_negatives.push(serde_json::json!({
                "id": id,
                "note": note,
                "file": rel,
            }));
        }

        let index = serde_json::json!({
            "schema_version": 1,
            "kind": role.as_str(),
            "backend": {
                "model_id": backend.0,
                "model_version": backend.1,
                "prompt_schema_version": backend.2,
            },
            "records": index_records,
            "negative": index_negatives,
        });
        std::fs::write(
            role_dir.join("index.json"),
            serde_json::to_string_pretty(&index).unwrap(),
        )
        .unwrap();
    }

    fn ocr_records() -> Vec<RecordSpec<'static>> {
        vec![
            RecordSpec {
                case: None,
                ..record(
                    "so2026-001-p1",
                    r#"{"blocks":[{"kind":"text","text":"第一页"}]}"#,
                )
            },
            RecordSpec {
                id: "irregular-p1",
                prompt: "同一提示词",
                asset: b"asset-bytes-irregular",
                output: r#"{"blocks":[{"kind":"text","text":"unknown"}]}"#,
                student_no: None,
                submission_stem: Some("IMG_20261104_001"),
                page_no: Some(1),
                question_ordinal: None,
                case: None,
            },
        ]
    }

    fn judge_record() -> RecordSpec<'static> {
        RecordSpec {
            id: "so2026-002-q16",
            prompt: "judge prompt",
            asset: b"judge-input",
            output: r#"{"judgement":"match","score_milli":5000,"reason":"正确","confidence":0.9}"#,
            student_no: Some("SO2026-002"),
            submission_stem: None,
            page_no: None,
            question_ordinal: Some(16),
            case: Some("normal"),
        }
    }

    fn tree() -> (
        tempfile::TempDir,
        Vec<RecordSpec<'static>>,
        RecordSpec<'static>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let ocr = ocr_records();
        let judge = judge_record();
        write_role(
            dir.path(),
            FixtureRole::Ocr,
            OCR_BACKEND,
            &ocr,
            &[(
                "missing-blocks",
                "缺少 blocks 必须拒绝",
                r#"{"schema_version":1}"#,
            )],
        );
        write_role(
            dir.path(),
            FixtureRole::Judge,
            JUDGE_BACKEND,
            std::slice::from_ref(&judge),
            &[(
                "match-with-wrong-score",
                "match 却 0 分必须拒绝",
                r#"{"judgement":"match","score_milli":0,"reason":"非法","confidence":0.9}"#,
            )],
        );
        (dir, ocr, judge)
    }

    #[test]
    fn loads_catalog_and_supports_three_match_levels() {
        let (dir, ocr, judge) = tree();
        let catalog = FixtureCatalog::load(dir.path()).unwrap();
        assert_eq!(catalog.len(), 3, "2 条 OCR + 1 条 judge");
        assert_eq!(catalog.roles(), vec![FixtureRole::Ocr, FixtureRole::Judge]);
        assert_eq!(catalog.negatives().len(), 2);
        assert_eq!(catalog.records().len(), 3);

        // ① 精确字节键：字节级回放
        let page = &catalog.by_role(FixtureRole::Ocr)[0];
        let exact = catalog.exact(page.entry.key).unwrap();
        assert_eq!(exact.key, page.entry.key);
        assert_eq!(
            exact.output_raw.as_bytes(),
            page.entry.output_raw.as_bytes()
        );

        // ② 组合键：不限定模型身份（跨注册表 id 的 E2E 用法）
        let combo = catalog
            .resolve(
                &FixtureLookup::new()
                    .role(FixtureRole::Ocr)
                    .hashes(page.prompt_sha256, page.asset_sha256),
            )
            .unwrap();
        assert_eq!(combo.output_raw, page.entry.output_raw);

        // ② 组合键：judge 角色
        let judge_loaded = &catalog.by_role(FixtureRole::Judge)[0];
        let combo = catalog
            .resolve(
                &FixtureLookup::new()
                    .role(FixtureRole::Judge)
                    .hashes(judge_loaded.prompt_sha256, judge_loaded.asset_sha256),
            )
            .unwrap();
        assert_eq!(combo.output_raw, judge_loaded.entry.output_raw);

        // ③ 语义标识：学生号+页码、文件 stem+页码、学生号+题号
        let by_student = catalog
            .resolve(&FixtureLookup::new().page(PageIdentity::by_student("SO2026-001", 1)))
            .unwrap();
        assert_eq!(by_student.output_raw, page.entry.output_raw);
        let by_stem = catalog
            .resolve(&FixtureLookup::new().page(PageIdentity::by_stem("IMG_20261104_001", 1)))
            .unwrap();
        assert_eq!(by_stem.output_raw, ocr[1].output, "未决页按文件 stem 命中");
        let by_question = catalog
            .resolve(&FixtureLookup::new().question(QuestionIdentity::new("SO2026-002", 16)))
            .unwrap();
        assert_eq!(by_question.output_raw, judge.output);

        // 未命中显式失败
        let miss = catalog
            .resolve(&FixtureLookup::new().page(PageIdentity::by_student("SO2026-999", 9)))
            .unwrap_err();
        assert!(matches!(miss, InferenceError::FixtureMiss { .. }));
    }

    #[test]
    fn combination_model_filter_is_verified() {
        let (dir, _ocr, _judge) = tree();
        let catalog = FixtureCatalog::load(dir.path()).unwrap();
        let page = &catalog.by_role(FixtureRole::Ocr)[0];
        let wrong_model = catalog
            .resolve(
                &FixtureLookup::new()
                    .role(FixtureRole::Ocr)
                    .model("another-model", "9.9")
                    .hashes(page.prompt_sha256, page.asset_sha256),
            )
            .unwrap_err();
        assert!(matches!(wrong_model, InferenceError::FixtureMiss { .. }));
        let right_model = catalog
            .resolve(
                &FixtureLookup::new()
                    .role(FixtureRole::Ocr)
                    .model(OCR_BACKEND.0, OCR_BACKEND.1)
                    .hashes(page.prompt_sha256, page.asset_sha256),
            )
            .unwrap();
        assert_eq!(right_model.output_raw, page.entry.output_raw);
    }

    #[test]
    fn tampered_asset_or_output_fails_closed() {
        let (dir, ocr, _judge) = tree();
        // 改一个输入字节 → sha256 与重算键都不再匹配
        let asset_path = dir
            .path()
            .join("ocr")
            .join("inputs")
            .join(format!("{}.png", ocr[0].id));
        let mut bytes = std::fs::read(&asset_path).unwrap();
        bytes.push(b'!');
        std::fs::write(&asset_path, &bytes).unwrap();
        let error = FixtureCatalog::load(dir.path()).unwrap_err();
        assert!(
            format!("{error}").contains("sha256"),
            "篡改输入必须因 sha256 不符被拒：{error}"
        );

        // 改输出文本 → 与索引 output_sha256 不符
        let (dir, ocr, _judge) = tree();
        let record_path = dir
            .path()
            .join("ocr")
            .join("pages")
            .join(format!("{}.json", ocr[0].id));
        let text = std::fs::read_to_string(&record_path).unwrap();
        std::fs::write(&record_path, text.replace("第一页", "被篡改的页面")).unwrap();
        let error = FixtureCatalog::load(dir.path()).unwrap_err();
        assert!(
            format!("{error}").contains("输出 sha256"),
            "篡改输出必须被拒：{error}"
        );
    }

    #[test]
    fn duplicate_combination_keys_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let mut records = ocr_records();
        // 同角色、同提示词、同输入 → 组合键重复
        records.push(RecordSpec {
            id: "duplicate-p1",
            ..record("duplicate-p1", r#"{"blocks":[]}"#)
        });
        write_role(dir.path(), FixtureRole::Ocr, OCR_BACKEND, &records, &[]);
        let error = FixtureCatalog::load_role(&dir.path().join("ocr")).unwrap_err();
        // 同一后端内「同提示词+同输入」必然同时重复精确键与组合键；这里断言拒绝即可
        //（组合键去重是防御性检查，见 `index_record`）。
        assert!(
            format!("{error}").contains("重复"),
            "重复夹具记录必须拒绝：{error}"
        );
    }

    #[test]
    fn negatives_are_loaded_but_never_replayed() {
        let (dir, _ocr, _judge) = tree();
        let catalog = FixtureCatalog::load(dir.path()).unwrap();
        assert_eq!(catalog.negatives().len(), 2);
        for negative in catalog.negatives() {
            assert_ne!(negative.note, "");
            assert_ne!(negative.id, "");
            // negative 的键不进入回放索引
            let miss = catalog.exact(negative.entry.key).unwrap_err();
            assert!(matches!(miss, InferenceError::FixtureMiss { .. }));
        }
    }

    #[test]
    fn fixture_backend_falls_back_to_combination_for_foreign_model_ids() {
        let (dir, ocr, _judge) = tree();
        let catalog = FixtureCatalog::load(dir.path()).unwrap();
        let backend = catalog.fixture_backend(FixtureRole::Ocr).unwrap();
        let exact = backend.replay(ocr[0].prompt, ocr[0].asset).unwrap();
        assert_eq!(exact.output_raw, ocr[0].output);

        // 调用方注册表模型 id 与录制 id 不同：精确键 miss → 组合键兜底
        let store = catalog.fixture_store(FixtureRole::Ocr).unwrap();
        let foreign = FixtureBackend::new("paddleocr-vl-1.6", "1.6", "ocr.blocks.v1", store)
            .with_catalog(Arc::new(catalog.clone()), FixtureRole::Ocr);
        let replayed = foreign.replay(ocr[1].prompt, ocr[1].asset).unwrap();
        assert_eq!(replayed.output_raw, ocr[1].output);
    }

    #[test]
    fn directory_without_index_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let error = FixtureCatalog::load(dir.path()).unwrap_err();
        assert!(format!("{error}").contains("没有 ocr/judge 夹具索引"));
    }
}
