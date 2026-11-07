-- Scanner (todo implementing-scanner).
--
-- volumes.held_removals: how many file rows the last scan found missing but did NOT remove,
-- because more than 20% of the volume (and at least 50 files) seemed to vanish at once. That
-- looks like an unmounted share or an emptied directory, not like a user deleting photos, so the
-- scanner waits for an explicit confirmation (`confirm_removals`). 0 when nothing is held.
ALTER TABLE volumes ADD COLUMN held_removals INTEGER NOT NULL DEFAULT 0;
