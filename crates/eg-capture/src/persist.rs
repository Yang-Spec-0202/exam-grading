//! 导入落库（D1）：一次导入批次的 `Submission`/`Page`/资产链接/审计**同一事务**写入。
//!
//! 规范：`SPEC-capture.md` §2 契约、§3 数据流；`SPEC-platform.md` §4.3（审计与业务写同事务）、
//! §4.4（资产内容寻址 + `asset_link`）；任务：T21–T22 的落库部分（D1 决策见
//! `docs/dev/research/capture.md` §7）。
//!
//! ## 事务边界（D1）
//!
//! - **资产字节与 `asset` 表行**：由 [`AssetStore`](eg_platform::AssetStore) 在导入过程中先行写入
//!   （内容寻址、幂等）；本模块只校验它们已存在。
//! - **业务行**（`submission` / `page` / `asset_link` / `audit_log`）：在**一个** `db.write` 事务内
//!   写入，顺序固定；任一失败整体回滚（不留半份提交、不留半个页、不留审计）。
//! - **任务阶段**：提交成功之后单独 `advance_stage`。此步失败时**业务数据已提交**，返回显式错误；
//!   因为导入本身已成功，重试落库会因主键冲突失败，需按错误里的提示单独修阶段。
//!
//! 因此落库失败（事务内）可整体重试；调用方不需要按页补偿。

use eg_domain::{
    AssetId, Page, QualityVerdict, Sha256Hex, Stage, Submission, TaskId, Timestamp, Ulid,
};
use eg_platform::repo;
use eg_platform::{NewAuditEntry, Platform};

use crate::error::CaptureError;
use crate::import::ImportOutcome;

/// `asset_link.owner_type`：页面。
pub const OWNER_PAGE: &str = "page";
/// `asset_link.owner_type`：参考答案页面资产（`KeyPageAsset`，不建模成 `Page`）。
pub const OWNER_KEY_PAGE_ASSET: &str = "key_page_asset";
/// `asset_link.role`：原图。
pub const ROLE_ORIGINAL: &str = "original";
/// `asset_link.role` 前缀：派生资产（后接 `Page.derived` 下标）。
///
/// `Page.derived` 是**压缩后**的定序列表（方向校正图、透视矫正图、二值图、缩略图，
/// 缺项跳过、去重），因此这里用位置下标命名，读方按同一列表顺序 + `SPEC-domain.md` §6
/// 的定序规则解释，不把 `derived_0` 硬编码成「方向校正图」。
pub const ROLE_DERIVED_PREFIX: &str = "derived_";

/// 落库结果计数（事务内真实写入行数，不是报告口径）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PersistSummary {
    /// 写入的提交数。
    pub submissions: u32,
    /// 写入的页面数（含被拒页）。
    pub pages: u32,
    /// 写入的资产链接数（页面原图/派生 + 参考答案页面资产）。
    pub asset_links: u32,
    /// 写入的审计条目数（批次 1 条 + 每提交 + 每页 + 每参考答案资产）。
    pub audit_entries: u32,
    /// 其中被质量门控拒收的页数。
    pub rejected_pages: u32,
    /// 参考答案页面资产链接数。
    pub key_assets: u32,
    /// 是否已把任务推进到「预处理」阶段。
    pub task_advanced: bool,
}

/// 落库请求。
#[derive(Debug, Clone, Copy)]
pub struct PersistRequest<'a> {
    /// 导入结果（提交/参考答案/批次报告）。
    pub outcome: &'a ImportOutcome,
    /// 关联任务；`Some` 时提交成功后把阶段推进到 [`Stage::Preprocess`]（采集完成）。
    pub task_id: Option<TaskId>,
}

/// 参考答案页面资产的链接计划（`ImportOutcome.key_assets` 的落库投影）。
#[derive(Debug, Clone)]
struct KeyLinkPlan {
    page_no: u32,
    original: AssetId,
    derived: Vec<AssetId>,
}

/// 事务内待写审计（id/时间在事务外预生成，保证事务内不做额外分配）。
struct PendingAudit {
    id: Ulid,
    at: Timestamp,
    entry: NewAuditEntry,
}

/// 事务返回值（`db.write` 要求 `Send + 'static`）。
#[derive(Debug, Clone, Copy, Default)]
struct Written {
    submissions: u32,
    pages: u32,
    asset_links: u32,
    rejected_pages: u32,
    key_assets: u32,
    audit_entries: u32,
}

/// 把一次导入结果落库：`Submission`/`Page`/`asset_link`/`audit_log` 单事务提交。
///
/// # Errors
///
/// - 资产未先经 `platform` 资产库落盘（违反 D1 调用顺序）→ [`CaptureError::PersistFailed`]；
/// - 任何数据库约束/序列化失败 → [`CaptureError::PersistFailed`]（事务已回滚，可整体重试）；
/// - 传了 `task_id` 但事务已提交、阶段推进失败 → [`CaptureError::PersistFailed`]
///   （**数据已提交**，错误里会写明，需单独修阶段而不是重试落库）。
pub fn persist(
    platform: &Platform,
    request: PersistRequest<'_>,
) -> Result<PersistSummary, CaptureError> {
    let outcome = request.outcome;
    let batch_id = outcome.report.batch_id;
    let kind = outcome.report.kind.as_str();

    // 1) 资产必须已落盘（D1：先资产、后业务单事务）。先整批校验，避免事务内才发现缺资产。
    let mut required: Vec<AssetId> = Vec::new();
    for submission in &outcome.submissions {
        for page in &submission.pages {
            required.push(page.asset);
            required.extend(page.derived.iter().copied());
        }
    }
    for key in &outcome.key_assets {
        required.push(key.source_asset);
        required.extend(key.derived.iter().copied());
    }
    for asset in &required {
        if !platform.assets().exists(*asset) {
            return Err(CaptureError::PersistFailed(format!(
                "资产未先落盘：{asset}（必须先经 platform 资产库写入，再落库业务行）"
            )));
        }
    }

    // 2) 事务外准备：克隆要写入的领域对象，预生成审计 id/时间与链接计划。
    let submissions: Vec<Submission> = outcome.submissions.clone();
    let key_links: Vec<KeyLinkPlan> = outcome
        .key_assets
        .iter()
        .map(|key| KeyLinkPlan {
            page_no: key.page_no,
            original: key.source_asset,
            derived: key.derived.clone(),
        })
        .collect();
    let at = platform.clock().now();
    let mut pending = Vec::new();

    let batch_note = format!(
        "导入批次：kind={kind}，文件 ok={}/{}，页面 total={}（good={} acceptable={} rejected={} failed={}），\
         pipeline={}，options_hash={}",
        outcome.report.files_ok,
        outcome.report.files_total,
        outcome.report.pages_total,
        outcome.report.pages_good,
        outcome.report.pages_acceptable,
        outcome.report.pages_rejected,
        outcome.report.pages_failed,
        outcome.report.pipeline_version,
        outcome.report.options_hash,
    );
    let mut batch_entry = NewAuditEntry::new("import.batch", "batch", batch_id.to_string())
        .with_note(batch_note)
        .with_hashes(
            None,
            Some(Sha256Hex::of(outcome.report.options_hash.as_bytes())),
        );
    if let Some(task_id) = request.task_id {
        batch_entry = batch_entry.with_task(task_id);
    }
    pending.push(PendingAudit {
        id: next_audit_id(platform)?,
        at,
        entry: batch_entry,
    });

    for submission in &submissions {
        // 审计备注刻意不含来源文件名/学号/姓名（可能是学生身份信息）。
        let mut entry =
            NewAuditEntry::new("submission.import", "submission", submission.id.to_string())
                .with_note(format!(
                    "新建提交：页数={}，状态={:?}",
                    submission.pages.len(),
                    submission.status
                ));
        if let Some(task_id) = request.task_id {
            entry = entry.with_task(task_id);
        }
        pending.push(PendingAudit {
            id: next_audit_id(platform)?,
            at,
            entry,
        });
        for page in &submission.pages {
            let (verdict, note) = page_audit_parts(page);
            let mut entry = NewAuditEntry::new("page.import", "page", page.id.to_string())
                .with_note(format!("第 {} 页：{note}", page.page_no))
                .with_hashes(None, Some(page_quality_hash(&verdict)));
            if let Some(task_id) = request.task_id {
                entry = entry.with_task(task_id);
            }
            pending.push(PendingAudit {
                id: next_audit_id(platform)?,
                at,
                entry,
            });
        }
    }
    for plan in &key_links {
        let owner_id = format!("{batch_id}:{}", plan.page_no);
        let mut entry =
            NewAuditEntry::new("key_page_asset.preprocess", OWNER_KEY_PAGE_ASSET, owner_id)
                .with_note(format!(
                    "参考答案页面资产：第 {} 页，派生资产 {} 项",
                    plan.page_no,
                    plan.derived.len()
                ));
        if let Some(task_id) = request.task_id {
            entry = entry.with_task(task_id);
        }
        pending.push(PendingAudit {
            id: next_audit_id(platform)?,
            at,
            entry,
        });
    }
    // 落库失败要整体回滚，因此审计条目与业务行必须同事务；这里先算出总数供摘要使用。
    let expected_audit = pending.len();

    // 3) 单事务写入业务行 + 审计（审计链在本事务内延伸）。
    let db = platform.db().clone();
    let audit = platform.audit().clone();
    let link_at = at;
    let written = db
        .write(move |tx| {
            let mut written = Written::default();
            for submission in &submissions {
                // `repo::insert_submission` 会同时写入该提交的全部页面（同事务）。
                repo::insert_submission(tx, submission)?;
                written.submissions += 1;
                for page in &submission.pages {
                    written.pages += 1;
                    if matches!(&page.quality.verdict, QualityVerdict::Rejected { .. }) {
                        written.rejected_pages += 1;
                    }
                    repo::link_asset(
                        tx,
                        OWNER_PAGE,
                        &page.id.to_string(),
                        ROLE_ORIGINAL,
                        page.asset,
                        link_at,
                    )?;
                    written.asset_links += 1;
                    for (index, asset) in page.derived.iter().enumerate() {
                        let role = format!("{ROLE_DERIVED_PREFIX}{index}");
                        repo::link_asset(
                            tx,
                            OWNER_PAGE,
                            &page.id.to_string(),
                            &role,
                            *asset,
                            link_at,
                        )?;
                        written.asset_links += 1;
                    }
                }
            }
            for plan in &key_links {
                let owner_id = format!("{batch_id}:{}", plan.page_no);
                repo::link_asset(
                    tx,
                    OWNER_KEY_PAGE_ASSET,
                    &owner_id,
                    ROLE_ORIGINAL,
                    plan.original,
                    link_at,
                )?;
                written.asset_links += 1;
                for (index, asset) in plan.derived.iter().enumerate() {
                    let role = format!("{ROLE_DERIVED_PREFIX}{index}");
                    repo::link_asset(tx, OWNER_KEY_PAGE_ASSET, &owner_id, &role, *asset, link_at)?;
                    written.asset_links += 1;
                }
                written.key_assets += 1;
            }
            for item in &pending {
                audit.append_in_tx(tx, item.id, item.at, &item.entry)?;
                written.audit_entries += 1;
            }
            Ok(written)
        })
        .map_err(|error| CaptureError::PersistFailed(format!("落库事务失败：{error}")))?;
    debug_assert_eq!(
        written.audit_entries,
        u32::try_from(expected_audit).unwrap_or(u32::MAX)
    );

    // 4) 提交后再推进任务阶段（失败不掩盖「数据已提交」这一事实）。
    let mut task_advanced = false;
    if let Some(task_id) = request.task_id {
        platform
            .tasks()
            .advance_stage(task_id, Stage::Preprocess)
            .map_err(|error| {
                CaptureError::PersistFailed(format!(
                    "数据已提交（提交 {} / 页 {} / 审计 {}），但任务阶段推进失败：{error}；\
                     不要重试落库，请单独修复任务阶段",
                    written.submissions, written.pages, written.audit_entries
                ))
            })?;
        task_advanced = true;
    }

    Ok(PersistSummary {
        submissions: written.submissions,
        pages: written.pages,
        asset_links: written.asset_links,
        audit_entries: written.audit_entries,
        rejected_pages: written.rejected_pages,
        key_assets: written.key_assets,
        task_advanced,
    })
}

/// 页面审计的结论与备注片段（备注不含文件名/身份信息）。
fn page_audit_parts(page: &Page) -> (String, String) {
    let derived = page.derived.len();
    match &page.quality.verdict {
        QualityVerdict::Good => (
            "good".to_owned(),
            format!("质量良好，派生资产 {derived} 项"),
        ),
        QualityVerdict::Acceptable => (
            "acceptable".to_owned(),
            format!("质量可接受，派生资产 {derived} 项"),
        ),
        QualityVerdict::Rejected { reason } => (
            format!("rejected:{reason}"),
            format!("质量拒收（{reason}），派生资产 {derived} 项，保留供教师复核/补拍"),
        ),
    }
}

/// 页面质量结论的稳定哈希（`Sha256Hex` 承载「当时质量结论」，供审计对照）。
fn page_quality_hash(verdict: &str) -> Sha256Hex {
    Sha256Hex::of(verdict.as_bytes())
}

/// 事务外预生成审计 id（审计 id 单调，事务内只做插入）。
fn next_audit_id(platform: &Platform) -> Result<Ulid, CaptureError> {
    platform
        .ids()
        .next_ulid()
        .map_err(|error| CaptureError::PersistFailed(format!("生成审计 id 失败：{error}")))
}
