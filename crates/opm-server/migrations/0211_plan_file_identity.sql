-- Shared snapshot identity for opt-in planners. No file FK: deleted ids must remain tombstones.
-- Rowid permits bounded generic cleanup. All FK and deletion probes have covering indexes.
CREATE TABLE plan_file_tracking (
    plan_id INTEGER NOT NULL PRIMARY KEY REFERENCES plans(id) ON DELETE CASCADE
) STRICT;
CREATE TABLE plan_file_identities (
    plan_id INTEGER NOT NULL REFERENCES plans(id) ON DELETE CASCADE,
    file_id INTEGER NOT NULL,
    live INTEGER NOT NULL CHECK (live IN (0, 1)),
    PRIMARY KEY (plan_id, file_id)
) STRICT;
CREATE INDEX plan_file_identities_file ON plan_file_identities(file_id, live);
CREATE INDEX plan_file_building ON plans(id) WHERE state = 'building';

CREATE TRIGGER plan_file_identity_deleted AFTER DELETE ON files
WHEN EXISTS (SELECT 1 FROM plan_file_tracking)
AND (EXISTS (SELECT 1 FROM plan_file_identities WHERE file_id = OLD.id AND live = 1)
OR EXISTS (
    SELECT 1 FROM plans p INDEXED BY plan_file_building
    JOIN plan_file_tracking t ON t.plan_id = p.id WHERE p.state = 'building'
))
BEGIN
    UPDATE plan_file_identities SET live = 0 WHERE file_id = OLD.id AND live = 1;
    INSERT INTO plan_file_identities(plan_id, file_id, live)
        SELECT p.id, OLD.id, 0 FROM plans p INDEXED BY plan_file_building
        JOIN plan_file_tracking t ON t.plan_id = p.id WHERE p.state = 'building'
        ON CONFLICT(plan_id, file_id) DO NOTHING;
END;
