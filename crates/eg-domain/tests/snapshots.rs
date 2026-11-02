//! 序列化快照：每个公开枚举/结构体一个快照（`SPEC-domain.md` §12、§14）。
//!
//! 目的：字段/枚举标签一旦变更即测试失败，迫使改动显式审核（规范 §10 序列化约定）。
//! 快照文件位于 `tests/snapshots/*.snap`；首次生成：
//! `INSTA_UPDATE=always cargo test -p eg-domain --test snapshots`。
//!
//! 约定：所有值使用固定 id 与固定时间，保证快照确定性；枚举快照覆盖全部变体。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::wildcard_imports,
    clippy::too_many_lines,
    clippy::similar_names
)]

use eg_domain::*;

// ---------------------------------------------------------------------------
// 夹具
// ---------------------------------------------------------------------------

fn ts() -> Timestamp {
    eg_domain::time::parse_rfc3339("2026-11-02T08:30:00Z").unwrap()
}

fn u(value: u128) -> Ulid {
    Ulid::from_u128(value)
}

fn milli(value: i64) -> MilliScore {
    MilliScore::from_milli(value).unwrap()
}

fn conf(value: f64) -> Confidence {
    Confidence::new(value).unwrap()
}

fn dec(text: &str) -> Decimal {
    Decimal::parse(text).unwrap()
}

fn bbox() -> BBox {
    BBox::new(0.1, 0.2, 0.3, 0.4).unwrap()
}

fn rubric_item(id: u128, criterion: &str, points: i64) -> RubricItem {
    RubricItem {
        id: RubricItemId::from_ulid(u(id)),
        criterion: criterion.to_owned(),
        points: milli(points),
        required: true,
        depends_on: Vec::new(),
    }
}

fn rubric() -> Rubric {
    Rubric {
        id: RubricId::from_ulid(u(31)),
        version: 2,
        items: vec![
            rubric_item(32, "写出公式", 1000),
            rubric_item(33, "得出结果", 1500),
        ],
        mode: ScoringMode::Stepwise,
        total: milli(2500),
        origin: RubricOrigin::StandardTemplate {
            template_id: TemplateId::new("national-1").unwrap(),
            template_version: 2,
            confirmed_by: LOCAL_TEACHER.to_owned(),
            confirmed_at: ts(),
        },
        partial_choice: None,
    }
}

fn partial_choice_rule() -> PartialChoiceRule {
    PartialChoiceRule {
        correct_option_count: 3,
        tiers: vec![
            PartialChoiceTier {
                selected_correct: 1,
                points: milli(2000),
                label: Some("选对 1 个".to_owned()),
            },
            PartialChoiceTier {
                selected_correct: 2,
                points: milli(4000),
                label: None,
            },
        ],
        wrong_selection_zero: true,
        source: PartialChoiceSource::Official,
        source_note: Some("2026 年全国卷评分细则".to_owned()),
        total_milli: milli(6000),
    }
}

fn concrete_key() -> ConcreteKey {
    ConcreteKey {
        primary: AnswerValue::Number {
            value: dec("2.5"),
            unit: Some("cm".to_owned()),
            tolerance: Some(Tolerance::new(Some(dec("0.01")), Some(1000)).unwrap()),
        },
        alternatives: vec![AnswerValue::Expression("\\frac{5}{2}".to_owned())],
        comparators: vec![ComparatorId::new("num_abs").unwrap()],
    }
}

fn question(key: AnswerKey) -> Question {
    Question {
        id: QuestionId::from_ulid(u(21)),
        paper_id: PaperId::from_ulid(u(11)),
        ordinal: 2,
        label: "17(2)".to_owned(),
        kind: QuestionKind::Calculation,
        max_score: milli(2500),
        stem: Some(ContentRef::Latex("求 $x$ 的值".to_owned())),
        key,
        rubric: Some(rubric()),
        subject_hint: Some(SubjectId::math()),
        knowledge_tags: vec!["函数".to_owned(), "方程".to_owned()],
    }
}

fn paper() -> Paper {
    Paper {
        id: PaperId::from_ulid(u(11)),
        classroom_id: ClassroomId::from_ulid(u(12)),
        title: "期中数学".to_owned(),
        subject: SubjectId::math(),
        total_score: milli(2500),
        questions: vec![question(AnswerKey::Missing)],
        key_version: 3,
        rubric_version: 2,
        created_at: ts(),
    }
}

fn engine(role: ModelRole, cloud: bool) -> EngineStamp {
    EngineStamp {
        role,
        backend: if cloud {
            BackendId::openai_compat()
        } else {
            BackendId::local_server()
        },
        model_id: "judge-4b".to_owned(),
        model_version: "1.0-q4".to_owned(),
        prompt_hash: Some(Sha256Hex::of(b"prompt")),
        params_hash: Sha256Hex::of(b"params"),
    }
}

fn evidence() -> Vec<EvidenceRef> {
    vec![
        EvidenceRef::Asset(AssetId::of(b"page-1")),
        EvidenceRef::Recognition(RecognitionId::from_ulid(u(41))),
        EvidenceRef::ComparatorTrace(ComparatorTraceId::from_ulid(u(42))),
        EvidenceRef::ModelCall(ModelCallId::from_ulid(u(43))),
    ]
}

fn judgement() -> Judgement {
    Judgement::Match {
        score: milli(2500),
        reason: "与参考答案一致".to_owned(),
        evidence: evidence(),
    }
}

fn grade_record() -> GradeRecord {
    GradeRecord {
        id: GradeId::from_ulid(u(51)),
        submission_id: SubmissionId::from_ulid(u(52)),
        question_id: QuestionId::from_ulid(u(21)),
        revision: 2,
        mode: GradeMode::Fast,
        route: RouteDecision::RuleThenModel,
        route_trace: RouteTrace {
            rule_id: "R3".to_owned(),
            inputs_hash: Sha256Hex::of(b"inputs"),
            classifier_call: Some(ModelCallId::from_ulid(u(43))),
            decided_at: ts(),
        },
        judgement: judgement(),
        awarded: milli(2500),
        max_score: milli(2500),
        confidence: conf(0.92),
        status: GradeStatus::NeedsReview,
        evidence: evidence(),
        engines: vec![engine(ModelRole::Judge, false)],
        created_at: ts(),
        reviewed_at: None,
        review_note: None,
        sampled: true,
    }
}

fn page() -> Page {
    Page {
        id: PageId::from_ulid(u(61)),
        submission_id: SubmissionId::from_ulid(u(52)),
        page_no: 1,
        asset: AssetId::of(b"original"),
        width: 1600,
        height: 1200,
        rotation_applied: Rotation::CW_90,
        exif_orientation: ExifOrientation::new(6).unwrap(),
        quality: PageQuality {
            blur_var: 180.5,
            exposure: 0.45,
            skew_deg: 1.2,
            resolution_ok: true,
            verdict: QualityVerdict::Acceptable,
        },
        derived: vec![AssetId::of(b"rotated"), AssetId::of(b"rectified")],
    }
}

fn submission() -> Submission {
    Submission {
        id: SubmissionId::from_ulid(u(52)),
        paper_id: PaperId::from_ulid(u(11)),
        classroom_id: ClassroomId::from_ulid(u(12)),
        student_id: Some(StudentId::from_ulid(u(13))),
        source_file_name: "张三_期中数学.jpg".to_owned(),
        source_stem: "张三_期中数学".to_owned(),
        pages: vec![page()],
        status: SubmissionStatus::Graded,
        created_at: ts(),
    }
}

fn block() -> Block {
    Block {
        kind: BlockKind::Formula,
        bbox: bbox(),
        text: "x^2+1".to_owned(),
        latex: Some("x^2+1".to_owned()),
        confidence: conf(0.97),
    }
}

fn recognition() -> Recognition {
    Recognition {
        id: RecognitionId::from_ulid(u(41)),
        page_id: PageId::from_ulid(u(61)),
        segment_id: Some(SegmentId::from_ulid(u(62))),
        blocks: vec![block()],
        text: "x^2+1".to_owned(),
        engine: engine(ModelRole::Ocr, false),
        confidence: conf(0.95),
        created_at: ts(),
    }
}

fn model_call() -> ModelCall {
    ModelCall {
        id: ModelCallId::from_ulid(u(43)),
        role: ModelRole::Judge,
        engine: engine(ModelRole::Judge, true),
        task_id: TaskId::from_ulid(u(44)),
        consent_id: Some(ConsentId::from_ulid(u(45))),
        input_hash: Sha256Hex::of(b"input"),
        output: serde_json::json!({"judgement": "match", "score": 2500}),
        latency_ms: 1234,
        cost_micros: 4567,
        created_at: ts(),
    }
}

fn student_answer() -> StudentAnswer {
    StudentAnswer {
        submission_id: SubmissionId::from_ulid(u(52)),
        question_id: QuestionId::from_ulid(u(21)),
        recognition_id: Some(RecognitionId::from_ulid(u(41))),
        segment_id: Some(SegmentId::from_ulid(u(62))),
        raw: raw_answer(),
        parts: vec![AnswerPart {
            page_id: PageId::from_ulid(u(61)),
            segment_id: SegmentId::from_ulid(u(62)),
            recognition_id: RecognitionId::from_ulid(u(41)),
            bbox: bbox(),
            text: "2.5".to_owned(),
        }],
        recognition_confidence: conf(0.88),
    }
}

fn raw_answer() -> RawAnswer {
    RawAnswer {
        text: "2.5".to_owned(),
        latex: Some("2.5".to_owned()),
        blocks: vec![block()],
        source: AnswerSource::Recognition,
    }
}

fn normalized_answer() -> NormalizedAnswer {
    NormalizedAnswer {
        text: "2.5".to_owned(),
        latex: Some("2.5".to_owned()),
        value: Some(AnswerValue::Number {
            value: dec("2.5"),
            unit: Some("cm".to_owned()),
            tolerance: None,
        }),
        steps: vec![NormalizedStep {
            ordinal: 1,
            text: "2.5".to_owned(),
            latex: Some("2.5".to_owned()),
        }],
    }
}

fn rubric_draft() -> RubricDraft {
    RubricDraft {
        id: RubricId::from_ulid(u(71)),
        question_id: QuestionId::from_ulid(u(21)),
        status: DraftStatus::PendingConfirmation,
        items: vec![
            RubricDraftItem {
                criterion: "写出公式".to_owned(),
                points: milli(1000),
                required: true,
                depends_on: Vec::new(),
            },
            RubricDraftItem {
                criterion: "得出结果".to_owned(),
                points: milli(1500),
                required: false,
                depends_on: vec![1],
            },
        ],
        raw_text: "写出公式得 1 分，得出结果得 1.5 分".to_owned(),
        origin: DraftOrigin::ModelStructured,
        model_call_id: Some(ModelCallId::from_ulid(u(72))),
        validation: vec![RubricIssue::warning("判据与模板占位相似")],
        created_at: ts(),
        confirmed_at: None,
    }
}

fn issue() -> RubricIssue {
    RubricIssue {
        severity: RubricIssueSeverity::Error,
        item: Some(RubricItemRef::Ordinal(2)),
        message: "分值合计不等于题目满分".to_owned(),
    }
}

macro_rules! snap {
    ($name:literal, $value:expr) => {
        insta::assert_json_snapshot!($name, $value);
    };
}

macro_rules! snap_debug {
    ($name:literal, $value:expr) => {
        insta::assert_debug_snapshot!($name, $value);
    };
}

// ---------------------------------------------------------------------------
// 快照：一个公开类型一个快照（按模块分组，便于审核）
// ---------------------------------------------------------------------------

#[test]
fn snapshot_value_objects() {
    snap!("ulid", u(0x0123_4567_89ab_cdef_0123_4567_89ab_cdef));
    snap!("student_id", StudentId::from_ulid(u(1)));
    snap!("classroom_id", ClassroomId::from_ulid(u(2)));
    snap!("paper_id", PaperId::from_ulid(u(3)));
    snap!("question_id", QuestionId::from_ulid(u(4)));
    snap!("submission_id", SubmissionId::from_ulid(u(5)));
    snap!("page_id", PageId::from_ulid(u(6)));
    snap!("segment_id", SegmentId::from_ulid(u(7)));
    snap!("recognition_id", RecognitionId::from_ulid(u(8)));
    snap!("grade_id", GradeId::from_ulid(u(9)));
    snap!("model_call_id", ModelCallId::from_ulid(u(10)));
    snap!("rubric_id", RubricId::from_ulid(u(11)));
    snap!("rubric_item_id", RubricItemId::from_ulid(u(12)));
    snap!("review_action_id", ReviewActionId::from_ulid(u(13)));
    snap!("task_id", TaskId::from_ulid(u(14)));
    snap!("consent_id", ConsentId::from_ulid(u(15)));
    snap!("comparator_trace_id", ComparatorTraceId::from_ulid(u(16)));
    snap!("milli_score", milli(2500));
    snap!("confidence", conf(0.85));
    snap!("content_hash", ContentHash::of(b"asset"));
    snap!("asset_id", AssetId::of(b"asset"));
    snap!("sha256_hex", Sha256Hex::of(b"payload"));
    snap!("decimal", dec("-12.340"));
    snap!("bbox", bbox());
    snap!("subject_id", SubjectId::math());
    snap!("comparator_id", ComparatorId::new("num_abs").unwrap());
    snap!("backend_id", BackendId::local_server());
    snap!("template_id", TemplateId::new("national-1").unwrap());
    snap!("calibration_id", CalibrationId::new("cal-2026").unwrap());
    snap!("rotation", Rotation::CW_270);
    snap!("exif_orientation", ExifOrientation::new(5).unwrap());
    snap!(
        "roster_source",
        [
            RosterSource::RosterImport,
            RosterSource::ManualAdd,
            RosterSource::FileNameInferred
        ]
    );
}

#[test]
fn snapshot_paper_module() {
    snap!(
        "classroom",
        Classroom {
            id: ClassroomId::from_ulid(u(2)),
            name: "高一(3)班".to_owned(),
            term: "2026 秋".to_owned(),
        }
    );
    snap!(
        "student",
        Student {
            id: StudentId::from_ulid(u(1)),
            classroom_id: ClassroomId::from_ulid(u(2)),
            student_no: "20260301".to_owned(),
            name: "张三".to_owned(),
            source: RosterSource::RosterImport,
        }
    );
    snap!("paper", paper());
    snap!("question", question(AnswerKey::Concrete(concrete_key())));
    snap!(
        "question_kind",
        [
            QuestionKind::SingleChoice,
            QuestionKind::MultipleChoice,
            QuestionKind::TrueFalse,
            QuestionKind::FillInBlank,
            QuestionKind::NumericOnly,
            QuestionKind::Calculation,
            QuestionKind::Proof,
            QuestionKind::ShortAnswer,
            QuestionKind::Essay,
            QuestionKind::Other
        ]
    );
    snap!(
        "content_ref",
        [
            ContentRef::Text("文本题干".to_owned()),
            ContentRef::Latex("$x^2$".to_owned()),
            ContentRef::Asset(AssetId::of(b"stem"))
        ]
    );
    snap!(
        "paper_template",
        PaperTemplate {
            paper_id: PaperId::from_ulid(u(11)),
            version: 3,
            page_count: 2,
            regions: vec![TemplateRegion {
                page_no: 1,
                question_id: QuestionId::from_ulid(u(21)),
                bbox: bbox(),
            }],
        }
    );
    snap!(
        "template_region",
        TemplateRegion {
            page_no: 2,
            question_id: QuestionId::from_ulid(u(22)),
            bbox: BBox::new(0.0, 0.5, 1.0, 0.5).unwrap(),
        }
    );
}

#[test]
fn snapshot_answer_module() {
    snap!(
        "answer_key",
        [
            AnswerKey::Concrete(concrete_key()),
            AnswerKey::RubricOnly,
            AnswerKey::ExplicitlyOmitted {
                note: Some("见教材".to_owned())
            },
            AnswerKey::Missing
        ]
    );
    snap!("concrete_key", concrete_key());
    snap!(
        "answer_value",
        [
            AnswerValue::Text("答案".to_owned()),
            AnswerValue::Number {
                value: dec("2.5"),
                unit: Some("cm".to_owned()),
                tolerance: None
            },
            AnswerValue::Expression("\\frac{5}{2}".to_owned()),
            AnswerValue::Options(vec!["A".to_owned(), "B".to_owned()]),
            AnswerValue::Steps(vec![AnswerValue::Text("x=1".to_owned())]),
            AnswerValue::Structured(serde_json::json!({"kind": "interval", "lo": 0, "hi": 1}))
        ]
    );
    snap!(
        "tolerance",
        Tolerance::new(Some(dec("0.01")), Some(1000)).unwrap()
    );
}

#[test]
fn snapshot_rubric_module() {
    snap!(
        "scoring_mode",
        [
            ScoringMode::Additive,
            ScoringMode::AllOrNothing,
            ScoringMode::Stepwise,
            ScoringMode::PartialChoice
        ]
    );
    snap!(
        "rubric_origin",
        [
            RubricOrigin::TeacherHandwritten {
                confirmed_by: LOCAL_TEACHER.to_owned(),
                confirmed_at: ts()
            },
            RubricOrigin::StandardTemplate {
                template_id: TemplateId::new("national-2").unwrap(),
                template_version: 1,
                confirmed_by: LOCAL_TEACHER.to_owned(),
                confirmed_at: ts()
            },
            RubricOrigin::ModelStructured {
                model_call_id: ModelCallId::from_ulid(u(43)),
                confirmed_by: LOCAL_TEACHER.to_owned(),
                confirmed_at: ts()
            }
        ]
    );
    snap!("rubric_item", rubric_item(32, "写出公式", 1000));
    snap!("rubric", rubric());
    snap!(
        "partial_choice_source",
        [
            PartialChoiceSource::Official,
            PartialChoiceSource::TemplateData,
            PartialChoiceSource::TeacherCustom
        ]
    );
    snap!(
        "partial_choice_tier",
        PartialChoiceTier {
            selected_correct: 1,
            points: milli(2000),
            label: Some("选对 1 个".to_owned()),
        }
    );
    snap!("partial_choice_rule", partial_choice_rule());
    snap!(
        "rule_text_source",
        [
            RuleTextSource::Handwritten,
            RuleTextSource::Template,
            RuleTextSource::Imported
        ]
    );
    snap!(
        "rule_text",
        RuleText {
            question_id: QuestionId::from_ulid(u(21)),
            text: "每步 1 分".to_owned(),
            source: RuleTextSource::Handwritten,
        }
    );
    snap!(
        "draft_status",
        [
            DraftStatus::Draft,
            DraftStatus::PendingConfirmation,
            DraftStatus::Confirmed,
            DraftStatus::Rejected
        ]
    );
    snap!(
        "draft_origin",
        [
            DraftOrigin::HandwrittenTransliteration,
            DraftOrigin::ModelStructured,
            DraftOrigin::TemplateApplied
        ]
    );
    snap!(
        "rubric_draft_item",
        RubricDraftItem {
            criterion: "写出公式".to_owned(),
            points: milli(1000),
            required: true,
            depends_on: vec![1],
        }
    );
    snap!("rubric_draft", rubric_draft());
}

#[test]
fn snapshot_page_module() {
    snap!(
        "stage",
        [
            Stage::Capture,
            Stage::Preprocess,
            Stage::Recognize,
            Stage::Align,
            Stage::Grade,
            Stage::Review,
            Stage::Export
        ]
    );
    snap!(
        "submission_status",
        [
            SubmissionStatus::Imported,
            SubmissionStatus::Preprocessed,
            SubmissionStatus::Recognized,
            SubmissionStatus::Aligned,
            SubmissionStatus::Graded,
            SubmissionStatus::Reviewed,
            SubmissionStatus::Exported,
            SubmissionStatus::Failed {
                stage: Stage::Recognize,
                message: "模型不可用".to_owned()
            }
        ]
    );
    snap!("submission", submission());
    snap!("page", page());
    snap!("page_quality", page().quality.clone());
    snap!(
        "quality_verdict",
        [
            QualityVerdict::Good,
            QualityVerdict::Acceptable,
            QualityVerdict::Rejected {
                reason: "模糊".to_owned()
            }
        ]
    );
    snap!(
        "segment",
        Segment {
            id: SegmentId::from_ulid(u(62)),
            page_id: PageId::from_ulid(u(61)),
            question_id: Some(QuestionId::from_ulid(u(21))),
            bbox: bbox(),
            crop: AssetId::of(b"crop"),
            method: SegmentMethod::LayoutDetection,
        }
    );
    snap!(
        "segment_method",
        [
            SegmentMethod::TemplateRegion,
            SegmentMethod::LayoutDetection,
            SegmentMethod::Manual
        ]
    );
}

#[test]
fn snapshot_recognition_module() {
    snap!(
        "model_role",
        [
            ModelRole::Ocr,
            ModelRole::Router,
            ModelRole::Judge,
            ModelRole::Structurer
        ]
    );
    snap!("engine_stamp", engine(ModelRole::Ocr, false));
    snap!("block", block());
    snap!(
        "block_kind",
        [
            BlockKind::Text,
            BlockKind::Formula,
            BlockKind::Table,
            BlockKind::Handwriting,
            BlockKind::Figure,
            BlockKind::Stamp,
            BlockKind::Barcode,
            BlockKind::HeaderFooter,
            BlockKind::Unknown
        ]
    );
    snap!("recognition", recognition());
}

#[test]
fn snapshot_evidence_module() {
    snap!("evidence_ref", evidence());
    snap!(
        "comparator_trace",
        ComparatorTrace {
            id: ComparatorTraceId::from_ulid(u(42)),
            comparator: ComparatorId::new("num_abs").unwrap(),
            verdict: ComparatorVerdict::Equivalent {
                note: "分数与小数等价".to_owned()
            },
            detail: serde_json::json!({"expected": "2.5", "actual": "2.50"}),
        }
    );
    snap!("model_call", model_call());
}

#[test]
fn snapshot_grade_module() {
    snap!(
        "route_decision",
        [
            RouteDecision::RuleOnly,
            RouteDecision::RuleThenModel,
            RouteDecision::ModelDirect,
            RouteDecision::RubricItemized,
            RouteDecision::Manual(ManualReason::AnswerKeyMissing)
        ]
    );
    snap!(
        "manual_reason",
        [
            ManualReason::AnswerKeyMissing,
            ManualReason::AnswerKeyOmitted,
            ManualReason::RubricMissing,
            ManualReason::RecognitionUnavailable,
            ManualReason::LowConfidence,
            ManualReason::OutOfScope,
            ManualReason::ConflictUnresolved,
            ManualReason::BackendUnavailable
        ]
    );
    snap!(
        "ungradable_reason",
        [
            UngradableReason::NotAnswered,
            UngradableReason::RecognitionUnavailable,
            UngradableReason::AnswerKeyIncomplete,
            UngradableReason::OutOfScope,
            UngradableReason::BackendUnavailable,
            UngradableReason::Timeout,
            UngradableReason::BudgetExceeded,
            UngradableReason::InvalidSchema,
            UngradableReason::Conflict,
            UngradableReason::PolicyBlocked
        ]
    );
    snap!(
        "judged_unit",
        [
            JudgedUnit::RubricItem(RubricItemId::from_ulid(u(32))),
            JudgedUnit::Choice("A".to_owned())
        ]
    );
    snap!(
        "judgement",
        [
            judgement(),
            Judgement::Partial {
                score: milli(2000),
                satisfied: vec![JudgedUnit::Choice("A".to_owned())],
                missing: vec![JudgedUnit::Choice("B".to_owned())],
                reason: "少选".to_owned()
            },
            Judgement::Mismatch {
                score: MilliScore::ZERO,
                reason: "答案不一致".to_owned(),
                evidence: evidence()
            },
            Judgement::Ungradable {
                reason: UngradableReason::Conflict
            }
        ]
    );
    snap!(
        "judge_outcome",
        JudgeOutcome {
            judgement: judgement(),
            confidence: conf(0.8),
            matched_alternative: Some(0),
            calibration_id: Some(CalibrationId::new("cal-2026").unwrap()),
        }
    );
    snap!("grade_mode", [GradeMode::Standard, GradeMode::Fast]);
    snap!(
        "route_trace",
        RouteTrace {
            rule_id: "R3".to_owned(),
            inputs_hash: Sha256Hex::of(b"inputs"),
            classifier_call: None,
            decided_at: ts(),
        }
    );
    snap!("grade_record", grade_record());
    snap!(
        "grade_status",
        [
            GradeStatus::AutoAccepted,
            GradeStatus::NeedsReview,
            GradeStatus::ManuallyReviewed,
            GradeStatus::Excluded
        ]
    );
    snap!(
        "review_reason_kind",
        [
            ReviewReasonKind::LowConfidence,
            ReviewReasonKind::Conflict,
            ReviewReasonKind::Ungradable,
            ReviewReasonKind::MissingEvidence,
            ReviewReasonKind::UncalibratedModel,
            ReviewReasonKind::BackendUnavailable,
            ReviewReasonKind::InvalidScore
        ]
    );
    snap!(
        "submission_exclusion",
        SubmissionExclusion {
            submission_id: SubmissionId::from_ulid(u(52)),
            reason: ExcludeReason::Absent,
            note: "缺考".to_owned(),
            at: ts(),
        }
    );
    snap!(
        "exclude_reason",
        [
            ExcludeReason::Absent,
            ExcludeReason::Invalid,
            ExcludeReason::Duplicate,
            ExcludeReason::OutOfScope,
            ExcludeReason::Other
        ]
    );
}

#[test]
fn snapshot_subject_module() {
    snap!(
        "answer_source",
        [
            AnswerSource::Recognition,
            AnswerSource::ManualCorrection,
            AnswerSource::ElectronicText
        ]
    );
    snap!("raw_answer", raw_answer());
    snap!(
        "answer_part",
        AnswerPart {
            page_id: PageId::from_ulid(u(61)),
            segment_id: SegmentId::from_ulid(u(62)),
            recognition_id: RecognitionId::from_ulid(u(41)),
            bbox: bbox(),
            text: "2.5".to_owned(),
        }
    );
    snap!("student_answer", student_answer());
    snap!(
        "normalized_step",
        NormalizedStep {
            ordinal: 1,
            text: "移项得 x=1".to_owned(),
            latex: Some("x=1".to_owned()),
        }
    );
    snap!("normalized_answer", normalized_answer());
    snap!(
        "step_evidence",
        StepEvidence {
            steps: vec![StepMatch {
                rubric_item: RubricItemId::from_ulid(u(32)),
                matched: true,
                evidence: Some(bbox()),
                note: None,
            }]
        }
    );
    snap!(
        "step_match",
        StepMatch {
            rubric_item: RubricItemId::from_ulid(u(33)),
            matched: false,
            evidence: None,
            note: Some("未定位到步骤".to_owned()),
        }
    );
    snap!(
        "subject_capabilities",
        SubjectCapabilities {
            kinds: vec![QuestionKind::NumericOnly, QuestionKind::MultipleChoice],
            comparators: vec![ComparatorId::new("num_abs").unwrap()],
            prompt_version: "math-judge-v1".to_owned(),
            supports_step_scoring: true,
        }
    );
    snap!(
        "rubric_issue_severity",
        [RubricIssueSeverity::Warning, RubricIssueSeverity::Error]
    );
    snap!(
        "rubric_item_ref",
        [
            RubricItemRef::Item(RubricItemId::from_ulid(u(32))),
            RubricItemRef::Ordinal(2)
        ]
    );
    snap!("rubric_issue", issue());
    snap!(
        "judge_context",
        JudgeContext {
            question: question(AnswerKey::RubricOnly),
            paper: paper(),
        }
    );
    snap!(
        "comparator_verdict",
        [
            ComparatorVerdict::Equal,
            ComparatorVerdict::Equivalent {
                note: "恒等变形".to_owned()
            },
            ComparatorVerdict::NotEqual {
                reason: "差值超出容差".to_owned()
            },
            ComparatorVerdict::Undecidable {
                reason: "超出支持范围".to_owned()
            }
        ]
    );
    snap!(
        "normalize_error",
        [
            NormalizeError::Unparsable {
                input: "x^".to_owned(),
                reason: "括号不配对".to_owned()
            },
            NormalizeError::Unsupported {
                reason: "微积分超出范围".to_owned()
            }
        ]
    );
}

#[test]
fn snapshot_contract_module() {
    snap!(
        "contract_violation",
        ContractViolation::NormalizeNotIdempotent
    );
    snap!(
        "chain_outcome",
        ChainOutcome {
            verdict: ComparatorVerdict::Equal,
            attempts: vec![(
                ComparatorId::new("num_abs").unwrap(),
                ComparatorVerdict::Equal
            )],
        }
    );
    snap!(
        "contract_case",
        ContractCase {
            question: question(AnswerKey::RubricOnly),
            key: concrete_key(),
            raw: raw_answer(),
            correct: normalized_answer(),
            wrong: Some(NormalizedAnswer {
                text: "3".to_owned(),
                latex: None,
                value: None,
                steps: Vec::new(),
            }),
            malformed: NormalizedAnswer {
                text: "\\bad".to_owned(),
                latex: None,
                value: None,
                steps: Vec::new(),
            },
        }
    );
    snap!(
        "contract_suite",
        ContractSuite {
            cases: vec![ContractCase {
                question: question(AnswerKey::RubricOnly),
                key: concrete_key(),
                raw: raw_answer(),
                correct: normalized_answer(),
                wrong: None,
                malformed: NormalizedAnswer {
                    text: "\\bad".to_owned(),
                    latex: None,
                    value: None,
                    steps: Vec::new(),
                },
            }],
        }
    );
}

#[test]
fn snapshot_errors_are_readable() {
    // 错误不是持久化类型，用 Debug 快照锁定中文文案与结构（规范 §11 可读性）。
    snap_debug!("error_score", ScoreError::Negative { value: -1 });
    snap_debug!("error_confidence", ConfidenceError::NaN);
    snap_debug!(
        "error_id",
        IdError::InvalidUlid {
            input: "short".to_owned(),
            reason: "长度必须为 26 字符".to_owned(),
        }
    );
    snap_debug!(
        "error_hash",
        HashError::InvalidHex {
            kind: "ContentHash",
            reason: "长度必须为 64 个字符",
        }
    );
    snap_debug!(
        "error_decimal",
        DecimalError::Format {
            input: "1.2.3".to_owned()
        }
    );
    snap_debug!("error_domain", DomainError::InvalidRevision { revision: 0 });
}
