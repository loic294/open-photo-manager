-- The per-volume trash folder is `.opm-trash` since the app was renamed from Advanced Photo Manager
-- to Open Photo Manager, before its first release. The check on `trash_items.trash_path` (0001,
-- rebuilt by 0047) still names the old folder, and SQLite cannot change a check in place, so the
-- table is rebuilt once more with the same columns, ids, indexes, triggers and AUTOINCREMENT counter.
--
-- No release ever wrote the old folder, so nothing on disk is renamed. Rows that a development
-- database still holds take the new prefix so they satisfy the check; the trash sync then treats
-- them like any row whose file is gone. No trigger fires while the rows are copied, so the counts of
-- `trash_volume_totals` (0048) stay as they are, and stay right: the rows are the same.
CREATE TABLE trash_items_rebuilt (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  volume_id  INTEGER NOT NULL REFERENCES volumes (id) ON DELETE CASCADE,
  batch      TEXT NOT NULL,
  orig_path  TEXT NOT NULL,                                           -- volume-relative RelPath strings
  trash_path TEXT NOT NULL CHECK (substr(trash_path, 1, 11) = '.opm-trash/'),
  is_dir     INTEGER NOT NULL DEFAULT 0 CHECK (is_dir IN (0, 1)),
  size       INTEGER NOT NULL DEFAULT 0,
  reason     TEXT,
  task_id    INTEGER,                                                 -- no FK: task history may be purged
  meta_json  TEXT CHECK (meta_json IS NULL OR json_valid(meta_json)),
  trashed_at INTEGER NOT NULL
) STRICT;

-- An id is never given out twice (0047): the new table starts from the old high-water mark.
INSERT INTO sqlite_sequence (name, seq)
SELECT 'trash_items_rebuilt', seq FROM sqlite_sequence WHERE name = 'trash_items';

INSERT INTO trash_items_rebuilt (id, volume_id, batch, orig_path, trash_path, is_dir, size, reason, task_id, meta_json, trashed_at)
SELECT id, volume_id, batch, orig_path, '.opm-trash/' || substr(trash_path, 12), is_dir, size, reason, task_id, meta_json, trashed_at
FROM trash_items
ORDER BY id;

DROP TABLE trash_items;
ALTER TABLE trash_items_rebuilt RENAME TO trash_items;

-- Dropping the table removed its indexes (0047, 0048) and triggers (0048); they come back unchanged.
CREATE UNIQUE INDEX trash_items_volume_trash_path ON trash_items (volume_id, trash_path);
CREATE INDEX trash_items_trashed_at ON trash_items (trashed_at DESC, id DESC);
CREATE INDEX trash_items_volume_trashed ON trash_items (volume_id, trashed_at DESC, id DESC);
CREATE INDEX trash_items_volume_batch ON trash_items (volume_id, batch, trashed_at DESC, id DESC, size, task_id, reason);

CREATE TRIGGER trash_items_totals_insert AFTER INSERT ON trash_items
BEGIN
  INSERT INTO trash_volume_totals (volume_id, items, bytes) VALUES (NEW.volume_id, 1, NEW.size)
  ON CONFLICT (volume_id) DO UPDATE SET items = items + 1, bytes = bytes + excluded.bytes;
END;

CREATE TRIGGER trash_items_totals_delete AFTER DELETE ON trash_items
BEGIN
  UPDATE trash_volume_totals SET items = items - 1, bytes = bytes - OLD.size WHERE volume_id = OLD.volume_id;
  DELETE FROM trash_volume_totals WHERE volume_id = OLD.volume_id AND items <= 0;
END;

CREATE TRIGGER trash_items_totals_update AFTER UPDATE OF volume_id, size ON trash_items
BEGIN
  UPDATE trash_volume_totals SET items = items - 1, bytes = bytes - OLD.size WHERE volume_id = OLD.volume_id;
  DELETE FROM trash_volume_totals WHERE volume_id = OLD.volume_id AND items <= 0;
  INSERT INTO trash_volume_totals (volume_id, items, bytes) VALUES (NEW.volume_id, 1, NEW.size)
  ON CONFLICT (volume_id) DO UPDATE SET items = items + 1, bytes = bytes + excluded.bytes;
END;
