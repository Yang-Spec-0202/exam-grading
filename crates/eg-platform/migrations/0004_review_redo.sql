-- 0004：复核命令重做栈（schema v4，task-66）。
--
-- 语义（与 review_undo 对称）：
--   * 撤销命令时，反向操作写入 review_undo；重做命令时，反向操作写入 review_redo；
--   * review_redo 只保存「可重做」的历史，LIFO 弹出后删除；
--   * 任何新的复核命令（非撤销/重做）必须在同一事务内调用 repo::clear_redo，
--     使「撤销后可重做、新操作后重做失效」成立；
--   * 表只由命令事务写入，重启后可读；平台不解释 payload。
--
-- 只追加迁移：不得修改历史迁移。

CREATE TABLE review_redo (
    id          TEXT PRIMARY KEY,
    command_id  TEXT NOT NULL,
    action      TEXT NOT NULL,
    entity_type TEXT NOT NULL,
    entity_id   TEXT NOT NULL,
    payload     TEXT NOT NULL,
    created_at  TEXT NOT NULL
);
CREATE INDEX idx_review_redo_created ON review_redo(created_at, id);
CREATE INDEX idx_review_redo_command ON review_redo(command_id);
