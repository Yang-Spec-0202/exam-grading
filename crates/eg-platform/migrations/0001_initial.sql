-- 0001_initial：首批表（SPEC-platform.md §4.1）
-- 约定：迁移只追加，禁止 DROP/RENAME 已发布列；JSON 文本列只追加不改写。
-- 时间列一律 TEXT（RFC3339 UTC）；分数列一律 INTEGER（千分之一分）。

CREATE TABLE app_meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE classroom (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL,
    term       TEXT NOT NULL,
    source     TEXT,
    body       TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE student (
    id           TEXT PRIMARY KEY,
    classroom_id TEXT NOT NULL REFERENCES classroom(id),
    student_no   TEXT NOT NULL UNIQUE,
    name         TEXT NOT NULL,
    source       TEXT NOT NULL,
    body         TEXT NOT NULL,
    created_at   TEXT NOT NULL
);
CREATE INDEX idx_student_classroom ON student(classroom_id);

CREATE TABLE paper (
    id             TEXT PRIMARY KEY,
    classroom_id   TEXT NOT NULL REFERENCES classroom(id),
    title          TEXT NOT NULL,
    subject        TEXT NOT NULL,
    total_score    INTEGER NOT NULL,
    key_version    INTEGER NOT NULL,
    rubric_version INTEGER NOT NULL,
    body           TEXT NOT NULL,
    created_at     TEXT NOT NULL
);

CREATE TABLE question (
    id         TEXT PRIMARY KEY,
    paper_id   TEXT NOT NULL REFERENCES paper(id),
    ordinal    INTEGER NOT NULL,
    label      TEXT NOT NULL,
    kind       TEXT NOT NULL,
    max_score  INTEGER NOT NULL,
    body       TEXT NOT NULL,
    UNIQUE (paper_id, ordinal)
);
CREATE INDEX idx_question_paper ON question(paper_id, ordinal);

CREATE TABLE paper_key_revision (
    paper_id    TEXT NOT NULL REFERENCES paper(id),
    key_version INTEGER NOT NULL,
    body        TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    PRIMARY KEY (paper_id, key_version)
);

CREATE TABLE rubric_revision (
    paper_id       TEXT NOT NULL REFERENCES paper(id),
    rubric_version INTEGER NOT NULL,
    body           TEXT NOT NULL,
    created_at     TEXT NOT NULL,
    PRIMARY KEY (paper_id, rubric_version)
);

CREATE TABLE rule_text (
    question_id TEXT PRIMARY KEY REFERENCES question(id),
    text        TEXT NOT NULL,
    source      TEXT NOT NULL,
    created_at  TEXT NOT NULL
);

CREATE TABLE rubric_draft (
    id            TEXT PRIMARY KEY,
    question_id   TEXT NOT NULL REFERENCES question(id),
    status        TEXT NOT NULL,
    origin        TEXT NOT NULL,
    model_call_id TEXT,
    validation    TEXT NOT NULL,
    body          TEXT NOT NULL,
    created_at    TEXT NOT NULL,
    confirmed_at  TEXT
);
CREATE INDEX idx_rubric_draft_question_status ON rubric_draft(question_id, status);

CREATE TABLE submission (
    id               TEXT PRIMARY KEY,
    paper_id         TEXT NOT NULL REFERENCES paper(id),
    classroom_id     TEXT NOT NULL REFERENCES classroom(id),
    student_id       TEXT REFERENCES student(id),
    source_file_name TEXT NOT NULL,
    source_stem      TEXT NOT NULL,
    status           TEXT NOT NULL,
    body             TEXT NOT NULL,
    created_at       TEXT NOT NULL
);
CREATE INDEX idx_submission_paper_student ON submission(paper_id, student_id);
CREATE INDEX idx_submission_status ON submission(status);

CREATE TABLE asset (
    id         TEXT PRIMARY KEY,
    byte_len   INTEGER NOT NULL,
    media_type TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE page (
    id              TEXT PRIMARY KEY,
    submission_id   TEXT NOT NULL REFERENCES submission(id),
    page_no         INTEGER NOT NULL,
    asset           TEXT NOT NULL REFERENCES asset(id),
    width           INTEGER NOT NULL,
    height          INTEGER NOT NULL,
    exif_orientation INTEGER NOT NULL,
    quality         TEXT NOT NULL,
    body            TEXT NOT NULL,
    created_at      TEXT NOT NULL,
    UNIQUE (submission_id, page_no)
);
CREATE INDEX idx_page_submission ON page(submission_id, page_no);

CREATE TABLE segment (
    id          TEXT PRIMARY KEY,
    page_id     TEXT NOT NULL REFERENCES page(id),
    question_id TEXT REFERENCES question(id),
    bbox        TEXT NOT NULL,
    method      TEXT NOT NULL,
    body        TEXT NOT NULL
);
CREATE INDEX idx_segment_page ON segment(page_id);
CREATE INDEX idx_segment_question ON segment(question_id);

CREATE TABLE recognition (
    id           TEXT PRIMARY KEY,
    page_id      TEXT NOT NULL REFERENCES page(id),
    segment_id   TEXT REFERENCES segment(id),
    confidence   REAL NOT NULL,
    engine       TEXT NOT NULL,
    body         TEXT NOT NULL,
    created_at   TEXT NOT NULL
);
CREATE INDEX idx_recognition_page ON recognition(page_id);

CREATE TABLE grade_record (
    id            TEXT PRIMARY KEY,
    submission_id TEXT NOT NULL REFERENCES submission(id),
    question_id   TEXT NOT NULL REFERENCES question(id),
    revision      INTEGER NOT NULL,
    mode          TEXT NOT NULL,
    sampled       INTEGER NOT NULL,
    route         TEXT NOT NULL,
    status        TEXT NOT NULL,
    confidence    REAL NOT NULL,
    awarded       INTEGER NOT NULL,
    body          TEXT NOT NULL,
    created_at    TEXT NOT NULL,
    UNIQUE (submission_id, question_id, revision)
);
CREATE INDEX idx_grade_submission_status ON grade_record(submission_id, status);

CREATE TABLE comparator_trace (
    id       TEXT PRIMARY KEY,
    grade_id TEXT NOT NULL REFERENCES grade_record(id),
    body     TEXT NOT NULL
);
CREATE INDEX idx_comparator_trace_grade ON comparator_trace(grade_id);

CREATE TABLE model_call (
    id           TEXT PRIMARY KEY,
    task_id      TEXT NOT NULL,
    consent_id   TEXT,
    role         TEXT NOT NULL,
    model_id     TEXT NOT NULL,
    input_hash   TEXT NOT NULL,
    cost_micros  INTEGER NOT NULL,
    latency_ms   INTEGER NOT NULL,
    body         TEXT NOT NULL,
    created_at   TEXT NOT NULL
);
CREATE INDEX idx_model_call_task ON model_call(task_id);
CREATE INDEX idx_model_call_consent ON model_call(consent_id);

CREATE TABLE review_action (
    id          TEXT PRIMARY KEY,
    grade_id    TEXT REFERENCES grade_record(id),
    before_hash TEXT NOT NULL,
    after_hash  TEXT NOT NULL,
    actor       TEXT NOT NULL,
    note        TEXT NOT NULL,
    body        TEXT NOT NULL,
    created_at  TEXT NOT NULL
);
CREATE INDEX idx_review_action_grade ON review_action(grade_id);

CREATE TABLE asset_link (
    owner_type TEXT NOT NULL,
    owner_id   TEXT NOT NULL,
    role       TEXT NOT NULL,
    asset_id   TEXT NOT NULL REFERENCES asset(id),
    created_at TEXT NOT NULL,
    PRIMARY KEY (owner_type, owner_id, role)
);
CREATE INDEX idx_asset_link_asset ON asset_link(asset_id);

CREATE TABLE task (
    id               TEXT PRIMARY KEY,
    kind             TEXT NOT NULL,
    state            TEXT NOT NULL,
    stage            TEXT,
    priority         INTEGER NOT NULL,
    idempotency_key  TEXT NOT NULL UNIQUE,
    payload          TEXT NOT NULL,
    attempts         INTEGER NOT NULL,
    max_attempts     INTEGER NOT NULL,
    next_run_at      TEXT NOT NULL,
    lease_owner      TEXT,
    lease_expires_at TEXT,
    last_error_class TEXT,
    last_error       TEXT,
    created_at       TEXT NOT NULL,
    updated_at       TEXT NOT NULL
);
CREATE INDEX idx_task_dispatch ON task(state, next_run_at, priority);

CREATE TABLE task_attempt (
    task_id      TEXT NOT NULL REFERENCES task(id),
    attempt_no   INTEGER NOT NULL,
    started_at   TEXT NOT NULL,
    ended_at     TEXT,
    error_class  TEXT,
    message      TEXT,
    PRIMARY KEY (task_id, attempt_no)
);

CREATE TABLE task_consent (
    consent_id      TEXT PRIMARY KEY,
    task_id         TEXT NOT NULL REFERENCES task(id),
    backend         TEXT NOT NULL,
    scope_hash      TEXT NOT NULL,
    budget_micros   INTEGER NOT NULL,
    state           TEXT NOT NULL,
    granted_at      TEXT NOT NULL,
    ended_at        TEXT
);
CREATE INDEX idx_consent_task_state ON task_consent(task_id, state);

CREATE TABLE audit_log (
    id          TEXT PRIMARY KEY,
    at          TEXT NOT NULL,
    actor       TEXT NOT NULL,
    action      TEXT NOT NULL,
    entity_type TEXT NOT NULL,
    entity_id   TEXT NOT NULL,
    before_hash TEXT,
    after_hash  TEXT,
    prev_hash   TEXT NOT NULL,
    chain_hash  TEXT NOT NULL,
    note        TEXT NOT NULL,
    consent_id  TEXT,
    task_id     TEXT
);
CREATE INDEX idx_audit_at ON audit_log(at);
CREATE INDEX idx_audit_entity ON audit_log(entity_type, entity_id);
