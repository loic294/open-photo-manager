-- Indexes and running totals of the trash page (todo building-trash-management). `trash_items` is
-- read through five shapes, and a trash of a million rows has to answer each of them without
-- reading the whole table:
--
--   * the item listing, newest first: `ORDER BY trashed_at DESC, id DESC` (the id breaks ties, so a
--     page boundary never repeats or skips a row), unfiltered or for one volume;
--   * the listing of one deletion (`volume_id` and `batch`), in the same order;
--   * the list of deletions (`GROUP BY volume_id, batch`) and the reasons of a deletion, which read
--     every row of the trash but only a few columns of it;
--   * the summary (items and bytes per volume), which every page of the trash carries;
--   * the retention sweeper, which asks for the oldest rows.
--
-- The plans are pinned by `trash/schema_tests.rs`. The price is three more index entries and one
-- small counter update per trashed item, which is nothing next to the rename and the journal rows
-- that trashing already costs. On a million rows in 2,500 deletions (release build) the summary
-- takes 0.1 ms, the first page of the listing 0.3 ms and the list of deletions 0.2 s.

-- The default listing and the retention sweeper ("anything older than T?" is one entry).
CREATE INDEX trash_items_trashed_at ON trash_items (trashed_at DESC, id DESC);

-- The same order for one volume. Without it a volume filter would walk the unique index and sort the
-- volume's whole trash.
CREATE INDEX trash_items_volume_trashed ON trash_items (volume_id, trashed_at DESC, id DESC);

-- One deletion is every row of a (volume, batch). The first four columns serve the listing of one
-- deletion in the listing order without a sort; the rest are what the grouped list of deletions and
-- the reasons of a deletion read, so none of them touches the table (the index is covering for
-- them).
CREATE INDEX trash_items_volume_batch ON trash_items (volume_id, batch, trashed_at DESC, id DESC, size, task_id, reason);

-- Items and bytes of every volume that holds trash. Summing a million rows takes about 90 ms, and
-- the summary rides on every request of the page, so the totals are kept as the rows change: one
-- row per volume, read in microseconds whatever the size of the trash. The triggers below are the
-- only writers; a volume with no item has no row, and a volume that goes takes its row with it.
--
-- `INSERT OR REPLACE` into `trash_items` would delete a row without running the delete trigger
-- (SQLite does that unless `recursive_triggers` is on), so nothing may use it; the sync with the
-- disk (`TrashService::sync`) recounts and repairs the totals anyway.
CREATE TABLE trash_volume_totals (
  volume_id INTEGER PRIMARY KEY REFERENCES volumes (id) ON DELETE CASCADE,
  items     INTEGER NOT NULL,
  bytes     INTEGER NOT NULL
) STRICT;

INSERT INTO trash_volume_totals (volume_id, items, bytes)
SELECT volume_id, COUNT(*), COALESCE(SUM(size), 0) FROM trash_items GROUP BY volume_id;

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

-- A row that changes volume or size moves its share from the old totals to the new ones.
CREATE TRIGGER trash_items_totals_update AFTER UPDATE OF volume_id, size ON trash_items
BEGIN
  UPDATE trash_volume_totals SET items = items - 1, bytes = bytes - OLD.size WHERE volume_id = OLD.volume_id;
  DELETE FROM trash_volume_totals WHERE volume_id = OLD.volume_id AND items <= 0;
  INSERT INTO trash_volume_totals (volume_id, items, bytes) VALUES (NEW.volume_id, 1, NEW.size)
  ON CONFLICT (volume_id) DO UPDATE SET items = items + 1, bytes = bytes + excluded.bytes;
END;
