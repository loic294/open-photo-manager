-- The rename planner stores the plan-time folder-local {count} in this nullable column.
ALTER TABLE move_items ADD COLUMN seq INTEGER;
