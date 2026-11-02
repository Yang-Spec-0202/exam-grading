-- 0003：复核队列索引 / 撤销栈 / 导出记录 / 提交级排除 / 判分幂等与断点 / 审计命令字段。
--
-- 只追加（`SPEC-platform.md` §4.1）：
-- - 不改 0001/0002 已发布的列/表；新增列一律可空（无默认语义猜测）；
-- - 不 DROP/RENAME；历史行保持可读（新列取 NULL）；
-- - 旧审计行的链哈希不因新列改变（新列仅在非空时进入规范化载荷，见 `audit` 模块）。
--
-- 背景（task-42 / platform v3，来自 review/report/grade 的反馈）：
-- 1) 复核队列需要跨提交、按模式/状态/抽检/原因/学生/题目过滤的「最新修订」查询，
--    以及稳定排序键游标分页；v1 只有按提交拉全部修订的 `list_grade_records`。
-- 2) 撤销/重做需要跨重启的命令栈（`review_undo`）。
-- 3) 导出需要可重放的导出记录（摘要哈希 + 参数/结果），并能按摘要比对重放结果。
-- 4) 提交级排除（`SubmissionExclusion`）此前只在内存/领域对象里，需持久化。
-- 5) 判分幂等：重启后重跑同一（提交, 题, 版本, 识别哈希, 引擎指纹, 模式）不得重复调用模型
--    或重复写判分记录；需要持久化幂等键与结果摘要。
-- 6) 判分断点：按提交/题粒度记录状态、尝试次数与引擎指纹，支持崩溃后恢复。
-- 7) 复核命令审计需要命令幂等键、提交/题实体、原因码、重跑范围、确认标记、抽检结论。

-- ---------------------------------------------------------------------------
-- 1) grade_record 查询索引（队列/统计）
-- ---------------------------------------------------------------------------

CREATE INDEX idx_grade_mode_status_sampled
    ON grade_record(mode, status, sampled);
CREATE INDEX idx_grade_status_question
    ON grade_record(status, question_id);
CREATE INDEX idx_grade_created_id
    ON grade_record(created_at, id);
-- 复核队列按学生过滤：提交表按 (student_id, status) 建索引。
CREATE INDEX idx_submission_student_status
    ON submission(student_id, status);

-- ---------------------------------------------------------------------------
-- 2) review_undo：复核命令撤销栈（持久化；按 created_at + id 稳定排序）
-- ---------------------------------------------------------------------------

CREATE TABLE review_undo (
    id          TEXT PRIMARY KEY,
    command_id  TEXT NOT NULL,
    action      TEXT NOT NULL,
    entity_type TEXT NOT NULL,
    entity_id   TEXT NOT NULL,
    payload     TEXT NOT NULL,
    created_at  TEXT NOT NULL
);
CREATE INDEX idx_review_undo_created ON review_undo(created_at, id);
CREATE INDEX idx_review_undo_command ON review_undo(command_id);

-- ---------------------------------------------------------------------------
-- 3) export_record：导出记录（审计 + 摘要 + 重放比较）
-- ---------------------------------------------------------------------------

CREATE TABLE export_record (
    id           TEXT PRIMARY KEY,
    kind         TEXT NOT NULL,
    format       TEXT NOT NULL,
    scope        TEXT NOT NULL,
    summary_hash TEXT NOT NULL,
    row_count    INTEGER NOT NULL,
    body         TEXT NOT NULL,
    created_at   TEXT NOT NULL,
    audit_id     TEXT
);
CREATE INDEX idx_export_created ON export_record(created_at, id);
CREATE INDEX idx_export_summary ON export_record(summary_hash);

-- ---------------------------------------------------------------------------
-- 4) submission_exclusion：提交级排除（与 grade_record 的题级排除区分）
-- ---------------------------------------------------------------------------

CREATE TABLE submission_exclusion (
    submission_id TEXT PRIMARY KEY REFERENCES submission(id),
    reason        TEXT NOT NULL,
    note          TEXT NOT NULL,
    at            TEXT NOT NULL,
    body          TEXT NOT NULL
);

-- ---------------------------------------------------------------------------
-- 5) grade_idempotency：判分幂等键（键唯一；结果摘要供重跑复用）
-- ---------------------------------------------------------------------------

CREATE TABLE grade_idempotency (
    key             TEXT PRIMARY KEY,
    submission_id   TEXT NOT NULL,
    question_id     TEXT NOT NULL,
    mode            TEXT NOT NULL,
    grade_id        TEXT,
    result_summary  TEXT NOT NULL,
    created_at      TEXT NOT NULL
);
CREATE INDEX idx_grade_idempotency_submission
    ON grade_idempotency(submission_id, question_id);

-- ---------------------------------------------------------------------------
-- 6) grade_progress：判分断点（提交级/题级；question_scope 空串 = 提交级）
-- ---------------------------------------------------------------------------

CREATE TABLE grade_progress (
    submission_id      TEXT NOT NULL,
    question_scope     TEXT NOT NULL,
    state              TEXT NOT NULL,
    attempts           INTEGER NOT NULL,
    engine_fingerprint TEXT,
    detail             TEXT,
    updated_at         TEXT NOT NULL,
    PRIMARY KEY (submission_id, question_scope)
);
CREATE INDEX idx_grade_progress_state
    ON grade_progress(state, updated_at);

-- ---------------------------------------------------------------------------
-- 7) grade_review_reason：判分记录的复核原因（队列按原因过滤；多值）
-- ---------------------------------------------------------------------------

CREATE TABLE grade_review_reason (
    grade_id    TEXT NOT NULL REFERENCES grade_record(id),
    reason_code TEXT NOT NULL,
    PRIMARY KEY (grade_id, reason_code)
);
CREATE INDEX idx_grade_review_reason_code ON grade_review_reason(reason_code);

-- ---------------------------------------------------------------------------
-- 8) audit_log：复核命令扩展字段（可空；旧行 NULL，不进链载荷）
-- ---------------------------------------------------------------------------

ALTER TABLE audit_log ADD COLUMN command_id TEXT;
ALTER TABLE audit_log ADD COLUMN submission_id TEXT;
ALTER TABLE audit_log ADD COLUMN question_id TEXT;
ALTER TABLE audit_log ADD COLUMN reason_code TEXT;
ALTER TABLE audit_log ADD COLUMN scope TEXT;
ALTER TABLE audit_log ADD COLUMN confirmed INTEGER;
ALTER TABLE audit_log ADD COLUMN sampling_verdict TEXT;

CREATE INDEX idx_audit_command ON audit_log(command_id);
CREATE INDEX idx_audit_submission_question ON audit_log(submission_id, question_id);
