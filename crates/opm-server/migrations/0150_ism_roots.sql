-- Roots delegated to ISM remain guarded even when the ISM service is unavailable.
CREATE TABLE ism_volume_roots (
    volume_id INTEGER PRIMARY KEY REFERENCES volumes (id) ON DELETE CASCADE,
    root_id TEXT NOT NULL UNIQUE,
    root_name TEXT NOT NULL
) STRICT;
