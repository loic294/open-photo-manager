-- Historical summaries deliberately have no file/volume/task foreign keys.
-- The short-lived queue is compacted by the statistics service, not retained as history.
CREATE TABLE statistics_segments (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    timezone TEXT NOT NULL,
    started_at INTEGER NOT NULL,
    ended_at INTEGER
) STRICT;
INSERT INTO statistics_segments(timezone,started_at)
VALUES(COALESCE((SELECT CASE WHEN json_valid(value) THEN json_extract(value,'$.statistics_timezone') END FROM settings WHERE key='app'),'America/Los_Angeles'),unixepoch());
CREATE TRIGGER statistics_timezone_insert AFTER INSERT ON settings
WHEN NEW.key='app' AND json_valid(NEW.value) AND COALESCE(json_extract(NEW.value,'$.statistics_timezone'),'America/Los_Angeles')
    <> (SELECT timezone FROM statistics_segments ORDER BY id DESC LIMIT 1)
BEGIN
    UPDATE statistics_segments SET ended_at=unixepoch() WHERE ended_at IS NULL;
    INSERT INTO statistics_segments(timezone,started_at)
    VALUES(COALESCE(json_extract(NEW.value,'$.statistics_timezone'),'America/Los_Angeles'),unixepoch());
END;
CREATE TRIGGER statistics_timezone_update AFTER UPDATE OF value ON settings
WHEN NEW.key='app' AND json_valid(NEW.value) AND COALESCE(json_extract(NEW.value,'$.statistics_timezone'),'America/Los_Angeles')
    <> (SELECT timezone FROM statistics_segments ORDER BY id DESC LIMIT 1)
BEGIN
    UPDATE statistics_segments SET ended_at=unixepoch() WHERE ended_at IS NULL;
    INSERT INTO statistics_segments(timezone,started_at)
    VALUES(COALESCE(json_extract(NEW.value,'$.statistics_timezone'),'America/Los_Angeles'),unixepoch());
END;
CREATE TABLE statistics_baselines (
    volume_id INTEGER PRIMARY KEY REFERENCES volumes(id) ON DELETE CASCADE,
    complete INTEGER NOT NULL CHECK (complete IN (0, 1))
) STRICT;
INSERT INTO statistics_baselines SELECT id, last_scan_at IS NOT NULL FROM volumes;
CREATE TRIGGER statistics_volume_insert AFTER INSERT ON volumes BEGIN
    INSERT INTO statistics_baselines VALUES (NEW.id, NEW.last_scan_at IS NOT NULL);
END;
CREATE TRIGGER statistics_volume_scanned AFTER UPDATE OF last_scan_at ON volumes
WHEN OLD.last_scan_at IS NULL AND NEW.last_scan_at IS NOT NULL BEGIN
    UPDATE statistics_baselines SET complete = 1 WHERE volume_id = NEW.id;
END;

CREATE TABLE statistics_identities (
    file_id INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
    identity TEXT NOT NULL DEFAULT (hex(randomblob(16)))
) STRICT;
INSERT INTO statistics_identities(file_id) SELECT id FROM files;

CREATE TABLE statistics_intents (
    key TEXT PRIMARY KEY,
    journal_id INTEGER REFERENCES fs_journal(id) ON DELETE SET NULL,
    op TEXT NOT NULL,
    state TEXT NOT NULL,
    src_volume_id INTEGER,
    src_path TEXT,
    dst_volume_id INTEGER,
    dst_path TEXT,
    subtree INTEGER NOT NULL DEFAULT 0,
    scan_generation INTEGER,
    created_at INTEGER NOT NULL,
    finished_at INTEGER
) STRICT;
CREATE UNIQUE INDEX statistics_intents_journal ON statistics_intents(journal_id);
CREATE INDEX statistics_intents_state ON statistics_intents(state,subtree,op);
CREATE INDEX statistics_intents_restore ON statistics_intents(dst_volume_id) WHERE subtree = 1;
-- Only outstanding folder-trash/restore membership is kept, never a permanent path history.
CREATE TABLE statistics_folder_members (
    operation_key TEXT NOT NULL REFERENCES statistics_intents(key) ON DELETE CASCADE,
    trash_item_id INTEGER REFERENCES trash_items(id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    PRIMARY KEY(operation_key,path)
) STRICT;
CREATE INDEX statistics_folder_members_trash ON statistics_folder_members(trash_item_id);
CREATE TRIGGER statistics_journal_insert AFTER INSERT ON fs_journal BEGIN
    DELETE FROM statistics_intents WHERE journal_id = NEW.id;
    INSERT INTO statistics_intents(key,journal_id,op,state,src_volume_id,src_path,dst_volume_id,dst_path,created_at)
    VALUES(hex(randomblob(16)),NEW.id,NEW.op,NEW.state,NEW.src_volume_id,NEW.src_path,NEW.dst_volume_id,NEW.dst_path,NEW.created_at);
END;
CREATE TRIGGER statistics_journal_update AFTER UPDATE ON fs_journal BEGIN
    UPDATE statistics_intents SET state = NEW.state, dst_volume_id = NEW.dst_volume_id,
        dst_path = NEW.dst_path, finished_at = NEW.finished_at WHERE journal_id = NEW.id;
END;
INSERT INTO statistics_intents(key,journal_id,op,state,src_volume_id,src_path,dst_volume_id,dst_path,created_at,finished_at)
SELECT hex(randomblob(16)),id,op,state,src_volume_id,src_path,dst_volume_id,dst_path,created_at,finished_at
FROM fs_journal WHERE state = 'pending';

CREATE TABLE statistics_context (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    operation_key TEXT
) STRICT;
INSERT INTO statistics_context VALUES (1, NULL);

CREATE TABLE statistics_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    at INTEGER NOT NULL DEFAULT (unixepoch()),
    timezone TEXT NOT NULL DEFAULT 'America/Los_Angeles',
    segment_id INTEGER NOT NULL DEFAULT 1,
    volume_id INTEGER NOT NULL,
    volume_name TEXT NOT NULL DEFAULT '',
    identity TEXT NOT NULL,
    action TEXT NOT NULL,
    count INTEGER NOT NULL DEFAULT 1,
    bytes INTEGER NOT NULL DEFAULT 0,
    path TEXT,
    operation_key TEXT,
    baseline INTEGER NOT NULL DEFAULT 0
) STRICT;
CREATE INDEX statistics_events_operation ON statistics_events(operation_key);
CREATE TRIGGER statistics_event_context AFTER INSERT ON statistics_events BEGIN
    UPDATE statistics_events SET
        timezone = (SELECT timezone FROM statistics_segments ORDER BY id DESC LIMIT 1),
        segment_id = (SELECT id FROM statistics_segments ORDER BY id DESC LIMIT 1),
        volume_name = COALESCE((SELECT name FROM volumes WHERE id = NEW.volume_id), ''),
        operation_key = COALESCE(NEW.operation_key,
            (SELECT operation_key FROM statistics_context WHERE id = 1),
            (SELECT key FROM statistics_intents i WHERE
                (i.state = 'pending' OR (i.op = 'restore' AND i.subtree = 1 AND i.state = 'done'))
                AND (
                    (NEW.action IN ('added','modified') AND i.dst_volume_id = NEW.volume_id
                        AND (i.state='pending' OR NEW.action='added')
                        AND (NEW.path = i.dst_path OR substr(NEW.path,1,length(i.dst_path)+1) = i.dst_path || '/')
                        AND (i.state='pending' OR EXISTS(
                            SELECT 1 FROM statistics_folder_members m WHERE m.operation_key=i.key AND m.path=NEW.path)))
                    OR (NEW.action = 'removed' AND i.src_volume_id = NEW.volume_id
                        AND (NEW.path = i.src_path OR substr(NEW.path,1,length(i.src_path)+1) = i.src_path || '/'))
                ) ORDER BY i.created_at DESC, i.journal_id DESC LIMIT 1))
    WHERE id = NEW.id;
END;

CREATE TRIGGER statistics_file_insert AFTER INSERT ON files BEGIN
    INSERT INTO statistics_identities(file_id) VALUES(NEW.id);
    INSERT INTO statistics_events(volume_id,identity,action,bytes,path,baseline)
    SELECT NEW.volume_id,identity,'added',NEW.size,
        CASE WHEN NEW.dir_path = '' THEN NEW.name ELSE NEW.dir_path || '/' || NEW.name END,
        NOT COALESCE((SELECT complete FROM statistics_baselines WHERE volume_id = NEW.volume_id),0)
    FROM statistics_identities WHERE file_id = NEW.id;
    DELETE FROM statistics_folder_members WHERE trash_item_id IS NULL
        AND path = CASE WHEN NEW.dir_path='' THEN NEW.name ELSE NEW.dir_path || '/' || NEW.name END
        AND operation_key IN (SELECT key FROM statistics_intents WHERE dst_volume_id=NEW.volume_id AND state='done' AND subtree=1);
END;
CREATE TRIGGER statistics_file_modified AFTER UPDATE OF size,mtime,kind ON files
WHEN OLD.size <> NEW.size OR OLD.mtime <> NEW.mtime OR OLD.kind <> NEW.kind BEGIN
    INSERT INTO statistics_events(volume_id,identity,action,bytes,path,baseline)
    SELECT NEW.volume_id,identity,'modified',NEW.size,
        CASE WHEN NEW.dir_path = '' THEN NEW.name ELSE NEW.dir_path || '/' || NEW.name END,
        NOT COALESCE((SELECT complete FROM statistics_baselines WHERE volume_id = NEW.volume_id),0)
    FROM statistics_identities WHERE file_id = NEW.id;
END;
CREATE TRIGGER statistics_file_removed BEFORE DELETE ON files BEGIN
    INSERT INTO statistics_events(volume_id,identity,action,bytes,path,baseline)
    SELECT OLD.volume_id,identity,'removed',OLD.size,
        CASE WHEN OLD.dir_path = '' THEN OLD.name ELSE OLD.dir_path || '/' || OLD.name END,
        NOT COALESCE((SELECT complete FROM statistics_baselines WHERE volume_id = OLD.volume_id),0)
    FROM statistics_identities WHERE file_id = OLD.id;
END;

CREATE TABLE statistics_daily (
    segment_id INTEGER NOT NULL,
    day TEXT NOT NULL,
    timezone TEXT NOT NULL,
    volume_id INTEGER NOT NULL,
    volume_name TEXT NOT NULL,
    action TEXT NOT NULL,
    count INTEGER NOT NULL,
    bytes INTEGER NOT NULL,
    PRIMARY KEY(segment_id,day,volume_id,action)
) STRICT;
CREATE TABLE statistics_seen (
    segment_id INTEGER NOT NULL,
    day TEXT NOT NULL,
    timezone TEXT NOT NULL,
    identity TEXT NOT NULL,
    action TEXT NOT NULL,
    PRIMARY KEY(segment_id,day,identity,action)
) STRICT;
CREATE INDEX statistics_daily_day ON statistics_daily(day,volume_id);
CREATE INDEX statistics_seen_day ON statistics_seen(day);
CREATE TABLE statistics_operations (
    key TEXT PRIMARY KEY REFERENCES statistics_intents(key) ON DELETE CASCADE,
    at INTEGER NOT NULL DEFAULT (unixepoch())
) STRICT;
CREATE TABLE statistics_snapshots (
    segment_id INTEGER NOT NULL,
    day TEXT NOT NULL,
    timezone TEXT NOT NULL,
    volume_id INTEGER NOT NULL,
    observed_at INTEGER NOT NULL,
    document TEXT NOT NULL CHECK(json_valid(document)),
    PRIMARY KEY(segment_id,day,volume_id)
) STRICT;
CREATE TABLE statistics_coverage (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    started_at INTEGER NOT NULL,
    last_seen_at INTEGER NOT NULL,
    stopped_at INTEGER
) STRICT;
CREATE INDEX statistics_snapshots_day ON statistics_snapshots(day,volume_id);
CREATE INDEX statistics_coverage_seen ON statistics_coverage(last_seen_at);
