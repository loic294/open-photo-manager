-- `trash_items.id` becomes AUTOINCREMENT (todo building-trash-management).
--
-- The trash page says "Empty trash" for the items the user is looking at: the request carries the
-- highest id of the summary on screen (`up_to_id`) and deletes nothing above it. That promise only
-- holds when an id is never given out twice. A plain `INTEGER PRIMARY KEY` hands out
-- `max(id) + 1`, so after the newest rows are restored or purged a later deletion can take an id
-- the user has already seen counted, and "everything up to id N" would destroy something that was
-- trashed after the user looked. AUTOINCREMENT remembers the highest id ever used in
-- `sqlite_sequence`, so a new row is always above every id a client has seen.
--
-- SQLite cannot change a primary key in place: the table is rebuilt with the same columns, checks and
-- unique index as `0001_core.sql`, and every row keeps its id. No table has a foreign key to
-- `trash_items` (the fs journal records paths), so none of them has to be touched.
CREATE TABLE trash_items_rebuilt (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  volume_id  INTEGER NOT NULL REFERENCES volumes (id) ON DELETE CASCADE,
  batch      TEXT NOT NULL,
  orig_path  TEXT NOT NULL,                                           -- volume-relative RelPath strings
  trash_path TEXT NOT NULL CHECK (substr(trash_path, 1, 11) = '.apm-trash/'),
  is_dir     INTEGER NOT NULL DEFAULT 0 CHECK (is_dir IN (0, 1)),
  size       INTEGER NOT NULL DEFAULT 0,
  reason     TEXT,
  task_id    INTEGER,                                                 -- no FK: task history may be purged
  meta_json  TEXT CHECK (meta_json IS NULL OR json_valid(meta_json)),
  trashed_at INTEGER NOT NULL
) STRICT;

INSERT INTO trash_items_rebuilt (id, volume_id, batch, orig_path, trash_path, is_dir, size, reason, task_id, meta_json, trashed_at)
SELECT id, volume_id, batch, orig_path, trash_path, is_dir, size, reason, task_id, meta_json, trashed_at
FROM trash_items
ORDER BY id;

DROP TABLE trash_items;
ALTER TABLE trash_items_rebuilt RENAME TO trash_items;

-- The unique index of 0001 (it also serves the foreign key to `volumes`).
CREATE UNIQUE INDEX trash_items_volume_trash_path ON trash_items (volume_id, trash_path);
