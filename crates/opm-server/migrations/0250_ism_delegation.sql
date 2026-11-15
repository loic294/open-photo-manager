-- Where a bound volume sits inside its ISM root ('' when the volume is the root itself).
ALTER TABLE ism_volume_roots ADD COLUMN root_prefix TEXT NOT NULL DEFAULT '';

-- Binding changes take effect at the next start, before any file operation can run. A NULL
-- root_id unbinds the volume; error says why a bind is still waiting.
CREATE TABLE ism_binding_changes (
    volume_id INTEGER PRIMARY KEY REFERENCES volumes (id) ON DELETE CASCADE,
    root_id TEXT,
    root_name TEXT NOT NULL DEFAULT '',
    root_prefix TEXT NOT NULL DEFAULT '',
    error TEXT,
    requested_at INTEGER NOT NULL
) STRICT;

-- Every batch sent to ISM, written before the request so a retry reuses the same key and body.
CREATE TABLE ism_operations (
    idempotency_key TEXT PRIMARY KEY,
    instance_id TEXT NOT NULL,
    request_json TEXT NOT NULL,
    operation_id TEXT,
    status TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
) STRICT;
