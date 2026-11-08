-- A rowid table allows the plan engine to collect expired rows in bounded chunks.
CREATE TABLE cleanup_dirs (
  plan_id INTEGER NOT NULL REFERENCES plans(id) ON DELETE CASCADE,
  path TEXT NOT NULL CHECK (path <> ''),
  depth INTEGER NOT NULL CHECK (depth >= 1),
  PRIMARY KEY (plan_id, path)
) STRICT;
CREATE INDEX cleanup_dirs_execution ON cleanup_dirs (plan_id, depth DESC, path);
