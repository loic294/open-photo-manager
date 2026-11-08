-- Rare dead snapshot members drive exact count subtraction without indexing live inserts.
CREATE INDEX plan_file_identities_dead
ON plan_file_identities(plan_id, file_id)
WHERE live = 0;
