-- Indexes of the trash page (todo building-trash-management). `trash_items` is read through four
-- shapes, and a trash of a million rows has to answer each of them from an index:
--
--   * the item listing, newest first: `ORDER BY trashed_at DESC, id DESC` (the id breaks ties, so a
--     page boundary never repeats or skips a row), unfiltered or for one volume;
--   * the listing of one deletion (`volume_id` and `batch`), in the same order;
--   * the summary (`GROUP BY volume_id`) and the list of deletions (`GROUP BY volume_id, batch`),
--     which read every row of the trash but only a few columns of it;
--   * the retention sweeper, which asks for the oldest rows.
--
-- Measured on 1,000,000 rows in 2,500 deletions of 400 items on two volumes (SQLite 3.4x, after ANALYZE):
-- first page 0.2 ms, summary 90 ms, one page of deletions 125 ms, a volume-filtered page and a deletion's page
-- under 1 ms; the plans are pinned by `trash/schema_tests.rs`. The price is three more index entries per
-- trashed item, which is nothing next to the rename and the journal rows that trashing already costs.

-- The default listing and the retention sweeper ("anything older than T?" is one entry).
CREATE INDEX trash_items_trashed_at ON trash_items (trashed_at DESC, id DESC);

-- The same order for one volume. Without it a volume filter would walk the unique index and sort the
-- volume's whole trash.
CREATE INDEX trash_items_volume_trashed ON trash_items (volume_id, trashed_at DESC, id DESC);

-- One deletion is every row of a (volume, batch). The first four columns serve the listing of one
-- deletion in the listing order without a sort; the rest are what the summary, the grouped list of
-- deletions and the reasons of a deletion read, so none of them touches the table (the index is
-- covering for them).
CREATE INDEX trash_items_volume_batch ON trash_items (volume_id, batch, trashed_at DESC, id DESC, size, task_id, reason);
