-- Stage queue (todo building-stage-queue, indexer/pending.rs).
--
-- One partial index per pipeline state column, holding only the rows that are still `pending` (0),
-- in the order a stage reads them: (volume_id, dir_path, name). A page of the queue is then one
-- range of at most `limit` entries, however many rows the volume has and however few of them are
-- pending, and the totals of a queue count that index alone. The existing (volume_id, *_state)
-- indexes stay: the counts of the index controls and the totals of a volume use them.
--
-- A statement that states the same condition, the literal `<column> = 0`, can use a partial index
-- (SQLite accepts a bound `0` only by preparing the statement again once the value is known), which
-- is how pending.rs writes its SQL.
-- Writing the state of a row (pending -> done) removes its entry, so the indexes shrink as the
-- stages catch up. The price is one more entry per pending row on every insert or reset.
CREATE INDEX files_meta_pending ON files (volume_id, dir_path, name) WHERE meta_state = 0;
CREATE INDEX files_hash_pending ON files (volume_id, dir_path, name) WHERE hash_state = 0;
CREATE INDEX files_thumb_pending ON files (volume_id, dir_path, name) WHERE thumb_state = 0;
