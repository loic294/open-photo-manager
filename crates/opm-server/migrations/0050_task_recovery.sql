ALTER TABLE tasks ADD COLUMN resumable INTEGER NOT NULL DEFAULT 0 CHECK (resumable IN (0, 1));

CREATE TABLE task_checkpoints (
    task_id INTEGER NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    key TEXT NOT NULL,
    value TEXT NOT NULL CHECK (json_valid(value)),
    PRIMARY KEY (task_id, key)
) STRICT;

ALTER TABLE fs_journal ADD COLUMN result_file_id INTEGER;
ALTER TABLE fs_journal ADD COLUMN result_live INTEGER NOT NULL DEFAULT 0 CHECK (result_live IN (0, 1));
ALTER TABLE fs_journal ADD COLUMN result_mtime INTEGER;

CREATE INDEX fs_journal_live_result ON fs_journal(result_file_id) WHERE result_live = 1;
CREATE TRIGGER fs_journal_result_deleted AFTER DELETE ON files BEGIN
    UPDATE fs_journal SET result_live = 0 WHERE result_file_id = OLD.id AND result_live = 1;
END;
