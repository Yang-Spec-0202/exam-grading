-- 0002：规则草稿负载列 + 生效规则按题目索引 + 授权额度用尽 + 审计扩展字段。
--
-- 只追加（`SPEC-platform.md` §4.1）：
-- - 不改 0001 已发布的列/表；新增列一律可空或带非空默认值；
-- - 不 DROP/RENAME；历史行保持可读（新列取默认值/NULL）。
--
-- 背景（task-37 / T-platform v2）：
-- 1) `eg_domain::RubricDraft` 不承载「题型/满分/计分方式/部分给分档位/模板引用」，
--    草稿确认与静态校验需要这些伴随字段，故在 `rubric_draft` 冗余成列
--    （领域对象仍以 `body` 版本化 JSON 为权威）。
-- 2) 生效规则按题查询：`rubric_revision` 增加 `question_id` 列；`paper.rubric_version`
--    仍是试卷级 CAS 版本基准，确认时同事务递增并写入本列，历史行（NULL）只读保留。
-- 3) 云端授权新增「额度用尽」状态：`spent_micros` 记录累计花费（微货币单位），
--    `exhausted_at` 记录用尽时间；状态值新增 `'exhausted'`（不新增枚举表）。
-- 4) 审计扩展：规则版本、模板引用、提示词哈希、模型调用 id 冗余成可空列；
--    审计哈希链对旧行保持兼容（新字段为空时不进入规范化载荷，见 `audit` 模块）。

-- ---------------------------------------------------------------------------
-- 1) rubric_draft：草稿伴随负载列
-- ---------------------------------------------------------------------------

-- 题目题型（`QuestionKind` 的 snake_case JSON 文本；v1 行为空 = 未记录）。
ALTER TABLE rubric_draft ADD COLUMN question_kind TEXT NOT NULL DEFAULT '';
-- 题目满分（千分之一分；v1 行为 0 = 未记录；与 `question.max_score` 冗余）。
ALTER TABLE rubric_draft ADD COLUMN max_score_milli INTEGER NOT NULL DEFAULT 0;
-- 计分方式（`ScoringMode` 的 snake_case JSON 文本；NULL = 未记录）。
ALTER TABLE rubric_draft ADD COLUMN scoring_mode TEXT;
-- 部分给分档位（`PartialChoiceRule` JSON；NULL = 不适用/未记录）。
ALTER TABLE rubric_draft ADD COLUMN partial_choice TEXT;
-- 模板引用：id/版本冗余成列便于审计查询，完整引用 JSON 保真。
ALTER TABLE rubric_draft ADD COLUMN template_id TEXT;
ALTER TABLE rubric_draft ADD COLUMN template_version INTEGER;
ALTER TABLE rubric_draft ADD COLUMN template_ref TEXT;

-- 按题目列全部草稿（按创建时间排序）与按状态检索。
CREATE INDEX idx_rubric_draft_question_created
    ON rubric_draft(question_id, created_at);
CREATE INDEX idx_rubric_draft_status_created
    ON rubric_draft(status, created_at);

-- ---------------------------------------------------------------------------
-- 2) rubric_revision：生效规则归属题目（历史行 NULL，只读保留）
-- ---------------------------------------------------------------------------

ALTER TABLE rubric_revision ADD COLUMN question_id TEXT REFERENCES question(id);
CREATE INDEX idx_rubric_revision_question
    ON rubric_revision(question_id, rubric_version DESC);

-- ---------------------------------------------------------------------------
-- 3) task_consent：额度累计与用尽时间
-- ---------------------------------------------------------------------------

-- 已花费（百万分之一货币单位；0 = 未花费；0 预算 = 本地/无费用，不累计判定用尽）。
ALTER TABLE task_consent ADD COLUMN spent_micros INTEGER NOT NULL DEFAULT 0;
-- 额度用尽时间（仅 `state = 'exhausted'` 时填写；不覆盖 ended_at）。
ALTER TABLE task_consent ADD COLUMN exhausted_at TEXT;

CREATE INDEX idx_consent_task_spent ON task_consent(task_id, spent_micros);

-- ---------------------------------------------------------------------------
-- 4) audit_log：规则确认/模板/模型整理扩展字段（可空，旧行 NULL）
-- ---------------------------------------------------------------------------

ALTER TABLE audit_log ADD COLUMN rubric_version INTEGER;
ALTER TABLE audit_log ADD COLUMN template_id TEXT;
ALTER TABLE audit_log ADD COLUMN template_version INTEGER;
ALTER TABLE audit_log ADD COLUMN template_ref TEXT;
ALTER TABLE audit_log ADD COLUMN prompt_hash TEXT;
ALTER TABLE audit_log ADD COLUMN model_call_id TEXT;

CREATE INDEX idx_audit_rubric ON audit_log(entity_type, entity_id, rubric_version);
CREATE INDEX idx_audit_model_call ON audit_log(model_call_id);
