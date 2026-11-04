//! 示例：加载仓库级 `fixtures/` 并演示夹具的三级匹配。
//!
//! 运行：
//!
//! ```text
//! cargo run -p eg-inference --example fixtures_load            # 默认 ../fixtures
//! cargo run -p eg-inference --example fixtures_load -- /path/to/fixtures
//! ```
//!
//! 输出只含条数/键/字节长度等元数据，不打印提示词或模型输出正文（避免把学生
//! 数据带到终端；夹具本身为合成数据，但仍按隐私规则处理）。

use std::error::Error;
use std::path::PathBuf;

use eg_inference::{FixtureCatalog, FixtureLookup, FixtureRole, PageIdentity, QuestionIdentity};

fn main() -> Result<(), Box<dyn Error>> {
    let root = std::env::args().nth(1).map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures"),
        PathBuf::from,
    );
    let catalog = FixtureCatalog::load(&root)?;
    println!("fixtures: {}", root.display());
    for role in catalog.roles() {
        let ids = catalog
            .backend_ids(role)
            .ok_or("夹具角色缺少 backend 元数据")?;
        println!(
            "- {}: {} 条记录，录制后端 {} {}（schema {}）",
            role.as_str(),
            catalog.by_role(role).len(),
            ids.model_id,
            ids.model_version,
            ids.prompt_schema_version
        );
    }
    println!(
        "- negative 样例：{} 条（只用于拒绝路径，永不回放）",
        catalog.negatives().len()
    );

    if let Some(record) = catalog.by_role(FixtureRole::Ocr).first() {
        let exact = catalog.exact(record.entry.key)?;
        println!(
            "OCR ① 精确键命中：key={} 输出 {} 字节",
            exact.key.to_hex(),
            exact.output_raw.len()
        );
        let combo = catalog.resolve(
            &FixtureLookup::new()
                .role(FixtureRole::Ocr)
                .hashes(record.prompt_sha256, record.asset_sha256),
        )?;
        println!(
            "OCR ② 组合键命中（提示词 sha256 + 输入 sha256）：输出 {} 字节",
            combo.output_raw.len()
        );
        if let Some(page) = &record.page {
            let by_page = if let Some(student_no) = &page.student_no {
                catalog.by_page(&PageIdentity::by_student(student_no.clone(), page.page_no))?
            } else if let Some(stem) = &page.submission_stem {
                catalog.by_page(&PageIdentity::by_stem(stem.clone(), page.page_no))?
            } else {
                exact
            };
            println!(
                "OCR ③ 语义标识命中：第 {} 页 → 输出 {} 字节",
                page.page_no,
                by_page.output_raw.len()
            );
        }
    }

    if let Some(record) = catalog.by_role(FixtureRole::Judge).first() {
        let question = record.question.as_ref().ok_or("judge 记录缺少题标识")?;
        let replay = catalog.by_question(&QuestionIdentity::new(
            question.student_no.clone(),
            question.question_ordinal,
        ))?;
        println!(
            "judge ③ 语义标识命中：{} 第 {} 题 → 输出 {} 字节（case={:?}）",
            question.student_no,
            question.question_ordinal,
            replay.output_raw.len(),
            record.case
        );
    }
    Ok(())
}
