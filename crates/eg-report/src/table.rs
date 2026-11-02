//! 报表行集（`SPEC-report.md` §3.5：工作表固定顺序与列定义）。
//!
//! 三种格式共用同一份 [`Table`] 行集，保证「XLSX/CSV/HTML 同列」；CSV 额外追加
//! 整数列（分数千分原值、得分率 ppm）以满足无损复算要求。

use crate::masking::{masked_name, masked_student_no};
use crate::rounding::{
    display_percent, display_score, display_two_decimals, round_ppm_to_tenth_percent,
};
use crate::settings::SAMPLING_METHOD;
use crate::source::mode_label;
use crate::stats::{Report, meta_rows};
use eg_domain::Timestamp;

/// 工作表（顺序即工作簿顺序；列序变更属破坏性修改）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Sheet {
    /// 元信息。
    Meta,
    /// 班级汇总。
    ClassSummary,
    /// 成绩单。
    Students,
    /// 题目统计。
    Questions,
    /// 选项分布。
    Options,
    /// 错题标签。
    Tags,
    /// 班级分布。
    Distribution,
    /// 快速抽检。
    Sampling,
}

impl Sheet {
    /// 工作表名（工作簿标签与 HTML 分节标题）。
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Meta => "元信息",
            Self::ClassSummary => "班级汇总",
            Self::Students => "成绩单",
            Self::Questions => "题目统计",
            Self::Options => "选项分布",
            Self::Tags => "错题标签",
            Self::Distribution => "班级分布",
            Self::Sampling => "快速抽检",
        }
    }

    /// 全部工作表（固定顺序）。
    pub(crate) const fn all() -> [Self; 8] {
        [
            Self::Meta,
            Self::ClassSummary,
            Self::Students,
            Self::Questions,
            Self::Options,
            Self::Tags,
            Self::Distribution,
            Self::Sampling,
        ]
    }
}

/// 列类型（决定 XLSX 数字格式与 CSV 无损整数列）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ColumnKind {
    /// 文本。
    Text,
    /// 分数（千分之一分，精确十进制）。
    Score,
    /// 得分率（0–1，ppm 存储，展示 0.1%）。
    Percent,
    /// 计数/排名等整数。
    Int,
}

/// 列定义。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Column {
    /// 列标题。
    pub(crate) title: String,
    /// 列类型。
    pub(crate) kind: ColumnKind,
}

/// 单元格。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Cell {
    /// 文本（枚举、状态、区间等）。
    Text(String),
    /// 分数（千分）。
    Score(i64),
    /// 得分率（ppm）。
    Percent(u32),
    /// 整数。
    Int(i64),
    /// 空（渲染为「—」）。
    Blank,
}

impl Cell {
    /// 展示文本（HTML/CSV/自检共用）。
    pub(crate) fn display(&self) -> String {
        match self {
            Self::Text(value) => value.clone(),
            Self::Score(value) => display_score(*value),
            Self::Percent(value) => display_percent(*value),
            Self::Int(value) => value.to_string(),
            Self::Blank => "—".to_owned(),
        }
    }
}

/// 一张工作表。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Table {
    /// 工作表。
    pub(crate) sheet: Sheet,
    /// 列。
    pub(crate) columns: Vec<Column>,
    /// 行。
    pub(crate) rows: Vec<Vec<Cell>>,
}

impl Table {
    /// 全部可见文本（脱敏自检用）。
    pub(crate) fn visible_text(&self) -> String {
        let mut out = String::with_capacity(self.rows.len() * self.columns.len() * 8);
        for column in &self.columns {
            out.push_str(&column.title);
            out.push('\n');
        }
        for row in &self.rows {
            for cell in row {
                out.push_str(&cell.display());
                out.push('\n');
            }
        }
        out
    }
}

fn column(title: &str, kind: ColumnKind) -> Column {
    Column {
        title: title.to_owned(),
        kind,
    }
}

fn text(value: impl Into<String>) -> Cell {
    Cell::Text(value.into())
}

fn blank() -> Cell {
    Cell::Blank
}

fn score(value: Option<i64>) -> Cell {
    value.map_or_else(blank, Cell::Score)
}

fn percent(value: Option<u32>) -> Cell {
    value.map_or_else(blank, Cell::Percent)
}

fn integer(value: Option<i64>) -> Cell {
    value.map_or_else(blank, Cell::Int)
}

/// 区分度展示：`0.60`（两位小数）或「不适用」。
fn discrimination_text(value: Option<i32>) -> Cell {
    value.map_or_else(
        || text("不适用"),
        |basis_points| {
            let sign = if basis_points < 0 { "-" } else { "" };
            let abs = basis_points.unsigned_abs();
            text(format!(
                "{sign}{}.{:02}",
                abs / 10_000,
                (abs % 10_000) / 100
            ))
        },
    )
}

/// 构建全部工作表。
pub(crate) fn build_tables(
    report: &Report,
    exported_at: Timestamp,
    masked: bool,
    naming_rule_version: u32,
) -> Vec<Table> {
    let mut tables = Vec::with_capacity(Sheet::all().len());
    tables.push(meta_table(report, exported_at, masked, naming_rule_version));
    tables.push(class_table(report));
    tables.push(student_table(report, masked));
    tables.push(question_table(report));
    tables.push(option_table(report));
    tables.push(tag_table(report));
    tables.push(distribution_table(report));
    tables.push(sampling_table(report, masked));
    tables
}

fn meta_table(
    report: &Report,
    exported_at: Timestamp,
    masked: bool,
    naming_rule_version: u32,
) -> Table {
    let rows = meta_rows(report, exported_at, masked, naming_rule_version)
        .into_iter()
        .map(|(key, value)| vec![text(key), text(value)])
        .collect();
    Table {
        sheet: Sheet::Meta,
        columns: vec![
            column("项目", ColumnKind::Text),
            column("值", ColumnKind::Text),
        ],
        rows,
    }
}

fn class_table(report: &Report) -> Table {
    let rows = report
        .class
        .iter()
        .map(|row| {
            vec![
                text(match row.mode {
                    Some(mode) => mode_label(mode),
                    None => "全部（对照，不产出混合指标）",
                }),
                Cell::Int(i64::try_from(row.valid_count).unwrap_or(i64::MAX)),
                Cell::Int(i64::try_from(row.absent_count).unwrap_or(i64::MAX)),
                Cell::Int(i64::try_from(row.excluded_count).unwrap_or(i64::MAX)),
                Cell::Int(i64::try_from(row.pending_review_count).unwrap_or(i64::MAX)),
                Cell::Int(i64::try_from(row.not_in_roster_count).unwrap_or(i64::MAX)),
                Cell::Int(i64::try_from(row.mixed_mode_count).unwrap_or(i64::MAX)),
                score(row.mean_milli),
                score(row.stddev_milli),
                score(row.max_milli),
                score(row.min_milli),
                percent(row.pass_ppm),
                percent(row.good_ppm),
                percent(row.excellent_ppm),
                Cell::Int(i64::try_from(row.full_mark_count).unwrap_or(i64::MAX)),
                Cell::Int(i64::try_from(row.sampled_record_count).unwrap_or(i64::MAX)),
            ]
        })
        .collect();
    let columns = vec![
        column("模式", ColumnKind::Text),
        column("有效样本数", ColumnKind::Int),
        column("缺考数", ColumnKind::Int),
        column("排除数", ColumnKind::Int),
        column("待复核数", ColumnKind::Int),
        column("名单外数", ColumnKind::Int),
        column("混合模式数", ColumnKind::Int),
        column("平均分", ColumnKind::Score),
        column("标准差", ColumnKind::Score),
        column("最高", ColumnKind::Score),
        column("最低", ColumnKind::Score),
        column("及格率", ColumnKind::Percent),
        column("良好率", ColumnKind::Percent),
        column("优秀率", ColumnKind::Percent),
        column("满分人数", ColumnKind::Int),
        column("抽检记录数", ColumnKind::Int),
    ];
    Table {
        sheet: Sheet::ClassSummary,
        columns,
        rows,
    }
}

fn student_table(report: &Report, masked: bool) -> Table {
    let mut rows = Vec::with_capacity(report.students.len());
    for student in &report.students {
        let display_index = student.ordinal.saturating_sub(1);
        let (student_no, name) = if masked {
            (masked_student_no(display_index), masked_name(display_index))
        } else {
            (student.student_no.clone(), student.name.clone())
        };
        let mut row = vec![
            Cell::Int(i64::try_from(student.ordinal).unwrap_or(i64::MAX)),
            text(student_no),
            text(name),
            text(student.mode.map_or("混合", mode_label)),
            text(student.outcome.label()),
            Cell::Score(student.effective_max_milli),
            Cell::Score(student.awarded_sum_milli),
            score(student.normalized_milli),
            Cell::Score(student.full_mark_milli),
            percent(student.rate_ppm),
            integer(student.rank.map(i64::from)),
        ];
        for award in &student.question_awards {
            row.push(score(*award));
        }
        row.push(text(student.tags.join("、")));
        rows.push(row);
    }
    let mut columns = vec![
        column("序号", ColumnKind::Int),
        column("学号", ColumnKind::Text),
        column("姓名", ColumnKind::Text),
        column("模式", ColumnKind::Text),
        column("状态", ColumnKind::Text),
        column("有效满分", ColumnKind::Score),
        column("得分", ColumnKind::Score),
        column("折算分", ColumnKind::Score),
        column("整卷满分", ColumnKind::Score),
        column("得分率", ColumnKind::Percent),
        column("同模式排名", ColumnKind::Int),
    ];
    for question in &report.paper.questions {
        columns.push(column(&question.label, ColumnKind::Score));
    }
    columns.push(column("错题标签", ColumnKind::Text));
    Table {
        sheet: Sheet::Students,
        columns,
        rows,
    }
}

fn question_table(report: &Report) -> Table {
    let rows = report
        .questions
        .iter()
        .map(|question| {
            vec![
                text(question.ordinal.to_string()),
                text(question.label.clone()),
                text(format!("{:?}", question.kind)),
                Cell::Score(question.max_score.as_milli()),
                Cell::Int(i64::try_from(question.standard.valid_count).unwrap_or(i64::MAX)),
                percent(question.standard.rate_ppm),
                discrimination_text(question.standard.discrimination_hundredths),
                Cell::Int(i64::try_from(question.fast.valid_count).unwrap_or(i64::MAX)),
                percent(question.fast.rate_ppm),
                discrimination_text(question.fast.discrimination_hundredths),
            ]
        })
        .collect();
    Table {
        sheet: Sheet::Questions,
        columns: vec![
            column("题号", ColumnKind::Text),
            column("标签", ColumnKind::Text),
            column("题型", ColumnKind::Text),
            column("满分", ColumnKind::Score),
            column("标准样本数", ColumnKind::Int),
            column("标准得分率", ColumnKind::Percent),
            column("标准区分度", ColumnKind::Text),
            column("快速样本数", ColumnKind::Int),
            column("快速得分率", ColumnKind::Percent),
            column("快速区分度", ColumnKind::Text),
        ],
        rows,
    }
}

fn option_table(report: &Report) -> Table {
    let rows = report
        .options
        .iter()
        .map(|row| {
            vec![
                text(row.label.clone()),
                text(mode_label(row.mode)),
                text(row.option.clone()),
                Cell::Int(i64::try_from(row.count).unwrap_or(i64::MAX)),
                percent(row.ppm),
                text(if row.correct { "是" } else { "否" }),
                Cell::Int(i64::try_from(row.blank_count).unwrap_or(i64::MAX)),
                Cell::Int(i64::try_from(row.unparsed_count).unwrap_or(i64::MAX)),
            ]
        })
        .collect();
    Table {
        sheet: Sheet::Options,
        columns: vec![
            column("题号", ColumnKind::Text),
            column("模式", ColumnKind::Text),
            column("选项", ColumnKind::Text),
            column("人数", ColumnKind::Int),
            column("占比", ColumnKind::Percent),
            column("是否正确答案", ColumnKind::Text),
            column("空计数", ColumnKind::Int),
            column("不确定计数", ColumnKind::Int),
        ],
        rows,
    }
}

fn tag_table(report: &Report) -> Table {
    let rows = report
        .tags
        .iter()
        .map(|row| {
            vec![
                text(row.tag.clone()),
                text(mode_label(row.mode)),
                Cell::Int(i64::try_from(row.question_count).unwrap_or(i64::MAX)),
                Cell::Int(i64::try_from(row.answer_count).unwrap_or(i64::MAX)),
                Cell::Int(i64::try_from(row.wrong_count).unwrap_or(i64::MAX)),
                percent(row.wrong_ppm),
                percent(row.lost_ppm),
            ]
        })
        .collect();
    Table {
        sheet: Sheet::Tags,
        columns: vec![
            column("标签", ColumnKind::Text),
            column("模式", ColumnKind::Text),
            column("涉及题数", ColumnKind::Int),
            column("作答次数", ColumnKind::Int),
            column("错题次数", ColumnKind::Int),
            column("错题率", ColumnKind::Percent),
            column("失分率", ColumnKind::Percent),
        ],
        rows,
    }
}

fn distribution_table(report: &Report) -> Table {
    let rows = report
        .distribution
        .iter()
        .map(|row| {
            vec![
                text(row.range.clone()),
                text(mode_label(row.mode)),
                Cell::Int(i64::try_from(row.count).unwrap_or(i64::MAX)),
                percent(row.ppm),
            ]
        })
        .collect();
    Table {
        sheet: Sheet::Distribution,
        columns: vec![
            column("区间", ColumnKind::Text),
            column("模式", ColumnKind::Text),
            column("人数", ColumnKind::Int),
            column("占比", ColumnKind::Percent),
        ],
        rows,
    }
}

fn sampling_table(report: &Report, masked: bool) -> Table {
    let rows = report
        .sampling
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let (submission, student) = if masked {
                (
                    format!("提交{:02}", index.saturating_add(1)),
                    masked_name(index),
                )
            } else {
                (row.submission_label.clone(), row.student_name.clone())
            };
            let _ = &row.student_no;
            vec![
                text(submission),
                text(student),
                text(row.sampled_label),
                text(row.status_label),
                text(row.converted_label),
            ]
        })
        .collect();
    Table {
        sheet: Sheet::Sampling,
        columns: vec![
            column("提交", ColumnKind::Text),
            column("学生", ColumnKind::Text),
            column("抽检标记", ColumnKind::Text),
            column("抽检状态", ColumnKind::Text),
            column("是否转标准复核", ColumnKind::Text),
        ],
        rows,
    }
}

/// 报表页脚说明（HTML 页脚；CSV 写入元信息分表）。
pub(crate) fn footer_note(report: &Report) -> String {
    format!(
        "抽样方法：{SAMPLING_METHOD}；抽检率 {}；低置信度权重 {}；标准与快速分列统计，不混合计算。\
         阈值：及格 {} / 优秀 {}。口径版本 {}；快照哈希 {}。",
        report.settings.display_sampling_rate(),
        report.settings.display_low_confidence_weight(),
        report.settings.display_pass(),
        report.settings.display_excellent(),
        report.settings.policy_version,
        report.snapshot_hash.to_hex()
    )
}

/// 得分率展示（0.1% 粒度）已由 [`display_percent`] 承担；此处提供两位小数工具给未来列扩展。
#[allow(dead_code)]
pub(crate) fn two_decimals(value: i64) -> String {
    display_two_decimals(value)
}

/// 阈值展示文本（元信息用；保留统一入口）。
#[allow(dead_code)]
pub(crate) fn percent_display(ppm: u32) -> String {
    display_percent(round_ppm_to_tenth_percent(ppm))
}
