-- Presence is enough for duplicate selection; descriptions remain in the original metadata.
ALTER TABLE files ADD COLUMN has_description INTEGER NOT NULL DEFAULT 0 CHECK (has_description IN (0, 1));
-- Re-extract existing media so the new selection rule is not silently ineffective.
UPDATE files SET meta_state = 0 WHERE kind IN ('image', 'video') AND meta_state = 1;
