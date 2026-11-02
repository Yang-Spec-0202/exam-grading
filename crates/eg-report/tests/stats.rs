//! 统计口径端到端测试（`SPEC-report.md` §7/§9：模式隔离、样本定义、区分度、守恒、重放）。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp,
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::assert_is_empty,
    clippy::field_reassign_with_default,
    clippy::redundant_closure
)]

mod common;

use common::Scenario;
use eg_domain::GradeMode;
use eg_report::{Report, ReportSettings, StudentOutcome, mode_label};

fn report() -> Report {
    let scenario = Scenario::build();
    match Report::build(&scenario.input, ReportSettings::default()) {
        Ok(report) => report,
        Err(error) => panic!("夹具必须合法：{error}"),
    }
}

#[test]
fn modes_are_reported_separately() {
    let report = report();
    let standard = report
        .class
        .iter()
        .find(|row| row.mode == Some(GradeMode::Standard))
        .expect("必须存在标准模式行");
    let fast = report
        .class
        .iter()
        .find(|row| row.mode == Some(GradeMode::Fast))
        .expect("必须存在快速模式行");
    let all = report
        .class
        .iter()
        .find(|row| row.mode.is_none())
        .expect("必须存在全部对照行");

    assert_eq!(standard.valid_count, 33);
    assert_eq!(fast.valid_count, 8);
    assert_eq!(all.valid_count, standard.valid_count + fast.valid_count);
    // 全部对照行不产出任何混合指标。
    assert!(all.mean_milli.is_none());
    assert!(all.pass_ppm.is_none());
    assert!(all.stddev_milli.is_none());

    // 标准 33 ≥ 30 → 有区分度；快速 8 < 30 → 不适用。
    let first = report.questions.first().expect("至少 1 题");
    assert!(first.standard.discrimination_hundredths.is_some());
    assert!(first.standard.discrimination_na.is_none());
    assert!(first.fast.discrimination_hundredths.is_none());
    assert_eq!(
        first.fast.discrimination_na,
        Some("样本不足（n<30），不适用")
    );
    assert_eq!(mode_label(GradeMode::Standard), "标准");

    // 抽检记录数：快速模式 3 份提交 × 10 道偶数题。
    assert_eq!(report.class[0].sampled_record_count, 30);
    assert_eq!(report.sampling.len(), 3);
    assert!(
        report
            .sampling
            .iter()
            .all(|row| row.sampled_label == "已抽检")
    );
}

#[test]
fn sample_classification_matches_spec() {
    let report = report();
    let outcome = |name: &str| {
        report
            .students
            .iter()
            .find(|student| student.name == name)
            .map(|student| student.outcome)
            .expect("夹具学生必须存在")
    };
    assert_eq!(outcome("张三"), StudentOutcome::NeedsReview);
    assert_eq!(outcome("李四"), StudentOutcome::Excluded);
    assert_eq!(outcome("王五"), StudentOutcome::MixedMode);
    assert_eq!(outcome("赵六"), StudentOutcome::Incomplete);
    assert_eq!(outcome("考生45"), StudentOutcome::Absent);
    assert!(
        report
            .students
            .iter()
            .any(|student| student.outcome == StudentOutcome::NotInRoster)
    );
    // 待复核/排除/混合模式/缺考/名单外不计入班级样本。
    let counted = report
        .students
        .iter()
        .filter(|student| student.outcome.in_class_sample())
        .count();
    assert_eq!(counted, 41);
    // 混合模式提交仍有单题记录：快速 5 题计入快速题目统计。
    let fifth = report
        .questions
        .iter()
        .find(|question| question.ordinal == 17)
        .expect("第 17 题");
    assert_eq!(fifth.fast.valid_count, 9);
}

#[test]
fn ranks_are_competition_style_and_mode_local() {
    let report = report();
    let standard: Vec<_> = report
        .students
        .iter()
        .filter(|student| student.mode == Some(GradeMode::Standard) && student.rank.is_some())
        .collect();
    assert_eq!(standard.len(), 33);
    // 最高分并列 1，其后按名次跳号。
    let best = standard
        .iter()
        .filter_map(|student| student.normalized_milli)
        .max()
        .unwrap();
    let best_count = standard
        .iter()
        .filter(|student| student.normalized_milli == Some(best))
        .count();
    assert!(best_count > 1);
    for student in &standard {
        if student.normalized_milli == Some(best) {
            assert_eq!(student.rank, Some(1));
        } else {
            let rank = student.rank.unwrap();
            assert!(rank > 1);
        }
    }
    // 缺考/待复核/排除/混合模式无排名。
    for student in &report.students {
        if !student.outcome.in_class_sample() {
            assert!(student.rank.is_none(), "{} 不应有排名", student.name);
        }
    }
}

#[test]
fn question_and_option_statistics_are_consistent() {
    let report = report();
    // 第 1 题标准模式样本：33 名整卷有效 + 待复核提交的该题 + 混合模式提交的该题。
    let first = report.questions.first().unwrap();
    assert_eq!(first.standard.valid_count, 35);
    assert_eq!(first.fast.valid_count, 8);
    assert!(first.standard.rate_ppm.unwrap() <= 1_000_000);
    assert!(first.standard.awarded_sum_milli <= first.standard.max_sum_milli);

    // 选项分布：每个（题, 模式）的人数之和 = 该题该模式有效样本数。
    let standard_options: Vec<_> = report
        .options
        .iter()
        .filter(|row| row.mode == GradeMode::Standard)
        .collect();
    assert_eq!(
        standard_options.iter().map(|row| row.count).sum::<usize>(),
        35
    );
    assert!(standard_options.iter().any(|row| row.option == "空"));
    assert!(standard_options.iter().any(|row| row.option == "不确定"));
    assert!(
        standard_options
            .iter()
            .any(|row| row.correct && row.option == "A")
    );
    // 空/不确定计数列在该题所有行一致。
    let blank = standard_options
        .iter()
        .find(|row| row.option == "空")
        .unwrap()
        .blank_count;
    assert!(standard_options.iter().all(|row| row.blank_count == blank));

    // 错题标签：标签集合覆盖题目知识点，错题次数与失分率计算自洽。
    assert!(!report.tags.is_empty());
    for row in &report.tags {
        assert!(row.wrong_count <= row.answer_count);
        assert!(row.wrong_ppm.unwrap() <= 1_000_000);
        assert!(row.lost_ppm.unwrap() <= 1_000_000);
        assert!(row.question_count >= 1);
    }
}

#[test]
fn distribution_bins_cover_whole_class() {
    let report = report();
    for (mode, expected) in [(GradeMode::Standard, 33), (GradeMode::Fast, 8)] {
        let bins: Vec<_> = report
            .distribution
            .iter()
            .filter(|row| row.mode == mode)
            .collect();
        assert_eq!(bins.len(), 10);
        assert_eq!(bins.iter().map(|row| row.count).sum::<usize>(), expected);
        // 末箱区间含 100%。
        assert_eq!(bins.last().unwrap().range, "90%–100%");
        assert!(bins.iter().all(|row| row.ppm.unwrap_or(0) <= 1_000_000));
    }
}

#[test]
fn build_is_deterministic_and_replayable() {
    let scenario = Scenario::build();
    let first = Report::build(&scenario.input, ReportSettings::default()).unwrap();
    let second = Report::build(&scenario.input, ReportSettings::default()).unwrap();
    assert_eq!(first, second);
    assert_eq!(first.snapshot_hash, second.snapshot_hash);
    assert_eq!(first.snapshot_hash.to_hex().len(), 64);
}

#[test]
fn rejects_inconsistent_snapshot() {
    let mut scenario = Scenario::build();
    scenario.input.paper.total_score = eg_domain::MilliScore::from_milli(99_000).expect("合法分数");
    let error = Report::build(&scenario.input, ReportSettings::default()).unwrap_err();
    assert!(matches!(error, eg_report::ReportError::Invariant(_)));
}

#[test]
fn rejects_invalid_settings() {
    let scenario = Scenario::build();
    let settings = ReportSettings {
        sampling_rate_ppm: 1,
        ..ReportSettings::default()
    };
    let error = Report::build(&scenario.input, settings).unwrap_err();
    assert!(matches!(error, eg_report::ReportError::Config(_)));
}

#[test]
fn empty_class_outputs_not_applicable() {
    let mut scenario = Scenario::build();
    scenario.input.students.clear();
    scenario.input.submissions.clear();
    scenario.input.records.clear();
    scenario.input.exclusions.clear();
    scenario.input.choices.clear();
    let report = Report::build(&scenario.input, ReportSettings::default()).unwrap();
    assert!(report.students.is_empty());
    assert!(report.class.iter().all(|row| row.valid_count == 0));
    assert!(report.class.iter().all(|row| row.mean_milli.is_none()));
    assert!(report.distribution.is_empty());
    assert_eq!(report.pending_count(), 0);
}
