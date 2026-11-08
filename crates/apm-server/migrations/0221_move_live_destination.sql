-- Count and page only movable rows without reading each snapshot's status from the table.
CREATE INDEX move_items_live_destination
ON move_items(plan_id, dst_volume_id, dst_dir, dst_name_norm, file_id)
WHERE status = 'move';

CREATE INDEX move_items_live_source
ON move_items(plan_id, file_id)
WHERE status = 'move';
