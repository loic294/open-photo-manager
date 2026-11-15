CREATE TABLE ism_connection (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    base_url TEXT NOT NULL,
    token_enc TEXT NOT NULL,
    instance_id TEXT NOT NULL,
    instance_name TEXT NOT NULL
) STRICT;
