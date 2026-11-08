CREATE TABLE move_plans (
    plan_id INTEGER PRIMARY KEY REFERENCES plans(id) ON DELETE CASCADE,
    conflict_policy TEXT NOT NULL CHECK (conflict_policy IN ('smart', 'skip', 'rename', 'replace')),
    name_max_len INTEGER NOT NULL
) STRICT;

-- No file FKs: short-lived snapshots survive deletion for file_gone details.
-- A rowid table is required by the generic engine's bounded garbage collector.
CREATE TABLE move_items (
    plan_id INTEGER NOT NULL REFERENCES plans(id) ON DELETE CASCADE,
    file_id INTEGER NOT NULL,
    dst_volume_id INTEGER NOT NULL,
    dst_dir TEXT NOT NULL,
    dst_name TEXT NOT NULL,
    dst_name_norm TEXT NOT NULL,
    wanted_name TEXT,
    status TEXT NOT NULL CHECK (status IN (
        'move', 'already_in_place', 'identical_at_destination', 'skipped_by_policy',
        'metadata_pending', 'pattern_failed', 'reserved_destination'
    )),
    counterpart_id INTEGER,
    replaced_id INTEGER,
    src_volume_id INTEGER NOT NULL,
    src_dir TEXT NOT NULL,
    src_name TEXT NOT NULL,
    src_size INTEGER NOT NULL,
    src_mtime INTEGER NOT NULL,
    PRIMARY KEY (plan_id, file_id)
) STRICT;
CREATE INDEX move_items_destination ON move_items(plan_id, dst_volume_id, dst_dir, dst_name_norm, file_id);
CREATE INDEX move_items_counterpart ON move_items(plan_id, counterpart_id) WHERE counterpart_id IS NOT NULL;
CREATE INDEX move_items_replaced ON move_items(plan_id, replaced_id) WHERE replaced_id IS NOT NULL;
