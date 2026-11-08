-- 0200_plans: the generic plan table of the plan engine.
--
-- Migration block 0200-0299 belongs to the plan engine (`building-plan-engine`) and to the planners built on it
-- (`implementing-duplicate-matching`, `implementing-move-planner`, `implementing-empty-folder-cleanup`).
-- This file holds ONLY what every kind of plan shares. A planner stores its own rows in a table of its own
-- (`plan_id INTEGER NOT NULL REFERENCES plans (id) ON DELETE CASCADE`, with an index that starts with `plan_id`)
-- and lists that table in `Planner::child_tables` so the engine purges it in chunks before it deletes the plan.
--
-- Conventions are those of 0001_core: STRICT tables, epoch SECONDS.

-- A plan is built once, shown as an overlay while it is `ready`, executed at most once and deleted when it
-- expires (2 hours after its last use). The ids are never reused (AUTOINCREMENT): a browse request that still
-- carries the id of a deleted plan must not land on a newer plan.
CREATE TABLE plans (
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  kind         TEXT NOT NULL CHECK (kind IN ('duplicates', 'move', 'cleanup')),
  -- 'building' rows are invisible to clients: the build runs inside the create request, and a row that is
  -- still 'building' after it died (crash, abort) is deleted at start-up and by the periodic sweep.
  state        TEXT NOT NULL CHECK (state IN ('building', 'ready', 'executing', 'executed')),
  params_json  TEXT NOT NULL CHECK (json_valid(params_json)),             -- the planner's parameters, never echoed
  stats_json   TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(stats_json)),   -- {"name": count}, set when the build ends
  warnings_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(warnings_json)),
  result_json  TEXT CHECK (result_json IS NULL OR json_valid(result_json)), -- the PlanResult, set once executed
  -- The `plan_execute` task. When the task history is purged before the plan expires, the plan keeps its result
  -- and simply loses the link (`plans_task` backs the foreign key).
  task_id      INTEGER REFERENCES tasks (id) ON DELETE SET NULL,
  created_at   INTEGER NOT NULL,
  expires_at   INTEGER NOT NULL                                             -- pushed back at every use of the plan
) STRICT;
CREATE INDEX plans_expires ON plans (expires_at);
CREATE INDEX plans_task ON plans (task_id);
