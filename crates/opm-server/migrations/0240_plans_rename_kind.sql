-- Plans are short-lived snapshots; discard their rows while preserving the never-reused id counter.
DELETE FROM duplicate_pairs;
DELETE FROM duplicate_matches;
DELETE FROM plan_file_tracking;
DELETE FROM plan_file_identities;
DELETE FROM move_plans;
DELETE FROM move_items;
DELETE FROM cleanup_dirs;
DELETE FROM plans;

CREATE TABLE plans_new (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  kind          TEXT NOT NULL CHECK (kind IN ('duplicates', 'move', 'cleanup', 'rename')),
  state         TEXT NOT NULL CHECK (state IN ('building', 'ready', 'executing', 'executed')),
  params_json   TEXT NOT NULL CHECK (json_valid(params_json)),
  stats_json    TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(stats_json)),
  warnings_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(warnings_json)),
  result_json   TEXT CHECK (result_json IS NULL OR json_valid(result_json)),
  task_id       INTEGER REFERENCES tasks (id) ON DELETE SET NULL,
  created_at    INTEGER NOT NULL,
  expires_at    INTEGER NOT NULL
) STRICT;

INSERT INTO sqlite_sequence (name, seq)
SELECT 'plans_new', seq FROM sqlite_sequence WHERE name = 'plans';

DROP TRIGGER plan_file_identity_deleted;
DROP TABLE plans;
ALTER TABLE plans_new RENAME TO plans;

CREATE INDEX plans_expires ON plans (expires_at);
CREATE INDEX plans_task ON plans (task_id);
CREATE INDEX plan_file_building ON plans(id) WHERE state = 'building';

CREATE TRIGGER plan_file_identity_deleted AFTER DELETE ON files
WHEN EXISTS (SELECT 1 FROM plan_file_tracking)
AND (EXISTS (SELECT 1 FROM plan_file_identities WHERE file_id = OLD.id AND live = 1)
OR EXISTS (
    SELECT 1 FROM plans p INDEXED BY plan_file_building
    JOIN plan_file_tracking t ON t.plan_id = p.id WHERE p.state = 'building'
))
BEGIN
    UPDATE plan_file_identities SET live = 0 WHERE file_id = OLD.id AND live = 1;
    INSERT INTO plan_file_identities(plan_id, file_id, live)
        SELECT p.id, OLD.id, 0 FROM plans p INDEXED BY plan_file_building
        JOIN plan_file_tracking t ON t.plan_id = p.id WHERE p.state = 'building'
        ON CONFLICT(plan_id, file_id) DO NOTHING;
END;
