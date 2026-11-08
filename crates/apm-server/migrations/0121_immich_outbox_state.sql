-- 0121_immich_outbox_state: how the outbox worker is doing, as one singleton row.
--
-- Migration block 0120-0139 belongs to todo `reflecting-file-ops-in-immich`. Written only by the outbox pass
-- (`crates/apm-server/src/immich/outbox/`); `GET /api/immich/status` reads it. The per-library scan times are kept in
-- memory: losing them only means one more (harmless) scan request after a restart.
CREATE TABLE immich_outbox_state (
  id                   INTEGER PRIMARY KEY CHECK (id = 1),
  last_run_at          INTEGER,                                              -- the end of the latest pass, whether it worked or not
  last_error           TEXT,                                                 -- why the latest pass failed as a whole; NULL after a success
  consecutive_failures INTEGER NOT NULL DEFAULT 0 CHECK (consecutive_failures >= 0) -- passes in a row that failed as a whole: the back-off
) STRICT;

INSERT INTO immich_outbox_state (id) VALUES (1);
