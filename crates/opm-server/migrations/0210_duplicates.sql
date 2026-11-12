-- Snapshot ids deliberately have no FK to files: vanished files are dropped by live joins.
-- Rowid is required by the generic engine's bounded child-table cleanup.
CREATE TABLE duplicate_pairs (
    plan_id INTEGER NOT NULL REFERENCES plans(id) ON DELETE CASCADE,
    source_id INTEGER NOT NULL,
    comparison_id INTEGER NOT NULL,
    PRIMARY KEY (plan_id, source_id, comparison_id)
) STRICT;
CREATE INDEX duplicate_pairs_comparison ON duplicate_pairs(plan_id, comparison_id);

-- Distinct membership avoids COUNT(DISTINCT) over millions of repeated pairs in browse.
CREATE TABLE duplicate_matches (
    plan_id INTEGER NOT NULL REFERENCES plans(id) ON DELETE CASCADE,
    side TEXT NOT NULL CHECK (side IN ('source', 'comparison')),
    file_id INTEGER NOT NULL,
    volume_id INTEGER NOT NULL,
    dir_path TEXT NOT NULL,
    live INTEGER NOT NULL DEFAULT 1 CHECK (live IN (0, 1)),
    PRIMARY KEY (plan_id, side, file_id)
) STRICT;
CREATE INDEX duplicate_matches_location ON duplicate_matches(plan_id, side, volume_id, live, dir_path, file_id);
CREATE INDEX duplicate_matches_file ON duplicate_matches(file_id);

-- Live location is derived, not a second snapshot of matching. Keep it current for moves.
-- Insert-time synchronization closes the read-snapshot -> bulk-write window of a build.
CREATE TRIGGER duplicate_matches_insert AFTER INSERT ON duplicate_matches
WHEN EXISTS (SELECT 1 FROM files WHERE id = NEW.file_id)
BEGIN
    UPDATE duplicate_matches SET (volume_id, dir_path) =
        (SELECT volume_id, dir_path FROM files WHERE id = NEW.file_id)
    WHERE rowid = NEW.rowid;
END;
CREATE TRIGGER duplicate_matches_relocate AFTER UPDATE OF volume_id, dir_path ON files
WHEN (OLD.volume_id <> NEW.volume_id OR OLD.dir_path <> NEW.dir_path)
AND EXISTS (SELECT 1 FROM duplicate_matches WHERE file_id = NEW.id AND live = 1)
BEGIN
    UPDATE duplicate_matches SET volume_id = NEW.volume_id, dir_path = NEW.dir_path
    WHERE file_id = NEW.id AND live = 1;
END;
CREATE TRIGGER duplicate_matches_vanished_insert AFTER INSERT ON duplicate_matches
WHEN NOT EXISTS (
    SELECT 1 FROM plan_file_tracking t JOIN plan_file_identities i ON i.plan_id = t.plan_id
    WHERE t.plan_id = NEW.plan_id AND i.file_id = NEW.file_id AND i.live = 1
)
BEGIN
    UPDATE duplicate_matches SET live = 0 WHERE rowid = NEW.rowid;
END;
-- Keep snapshot ids for file_gone execution details, but exclude them from the live count cache.
CREATE TRIGGER duplicate_matches_vanished AFTER DELETE ON files
WHEN EXISTS (SELECT 1 FROM duplicate_matches WHERE file_id = OLD.id AND live = 1)
BEGIN
    UPDATE duplicate_matches SET live = 0 WHERE file_id = OLD.id AND live = 1;
END;
-- A tombstoned membership NEVER revives when SQLite reuses an id on restore/insertion.
-- A fresh plan is required for that new row; overlays and survivor checks require live = 1.
