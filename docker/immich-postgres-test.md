# Explicit Immich v3.3.3 PostgreSQL gate

This **isolated integration setup** runs the real v3.3.3 server's Kysely migrations
with release-pinned PostgreSQL/vector and Valkey images. No synthetic replacement
schema is accepted. The adapter requires the exact migration list ending in
`1790693088454-AddPersonUserTableSharedBySharedWithConstraint`, enum values
`sha1`/`sha1-path`, checksum uniqueness and enabled timestamp/cursor triggers.

Never point this gate at an existing Immich installation. It writes synthetic
assets and intentionally reproduces a checksum collision. Its disposable database
name and explicit marker are mandatory. Ordinary Rust tests do not require Docker;
this command is **required before landing** and fails when configuration,
PostgreSQL, TLS or schema is absent. An ignored test is not a passed gate.

Check Docker VM writable capacity and that localhost port 25432 is free first.
Do not prune shared images, containers, volumes or caches. Use only this named
Compose project's data and remove it afterwards.

From the isolated checkout:

```sh
export OPM_IMMICH_TEST_DIR="$PWD/target/immich-postgres-test"
export OPM_IMMICH_TEST_PASSWORD="$(openssl rand -hex 24)"
mkdir -p "$OPM_IMMICH_TEST_DIR/tls"
openssl req -x509 -newkey rsa:2048 -nodes -days 1 \
  -keyout "$OPM_IMMICH_TEST_DIR/tls/ca.key" \
  -out "$OPM_IMMICH_TEST_DIR/tls/ca.crt" \
  -subj '/CN=OPM disposable test CA' \
  -addext 'basicConstraints=critical,CA:TRUE'
openssl req -new -newkey rsa:2048 -nodes \
  -keyout "$OPM_IMMICH_TEST_DIR/tls/server.key" \
  -out "$OPM_IMMICH_TEST_DIR/tls/server.csr" \
  -subj '/CN=localhost' -addext 'subjectAltName=DNS:localhost' \
  -addext 'basicConstraints=critical,CA:FALSE' \
  -addext 'extendedKeyUsage=serverAuth'
openssl x509 -req -in "$OPM_IMMICH_TEST_DIR/tls/server.csr" \
  -CA "$OPM_IMMICH_TEST_DIR/tls/ca.crt" \
  -CAkey "$OPM_IMMICH_TEST_DIR/tls/ca.key" -CAcreateserial \
  -days 1 -copy_extensions copy -out "$OPM_IMMICH_TEST_DIR/tls/server.crt"
chmod 600 "$OPM_IMMICH_TEST_DIR/tls/server.key" "$OPM_IMMICH_TEST_DIR/tls/ca.key"
docker compose -f docker/immich-postgres-test.compose.yml up -d --wait
docker compose -f docker/immich-postgres-test.compose.yml logs --no-color server
docker compose -f docker/immich-postgres-test.compose.yml exec -T server \
  node -p 'require("/usr/src/app/server/package.json").version'
docker image inspect ghcr.io/immich-app/immich-server:v3.3.3 \
  --format '{{json .RepoDigests}}'
```

Require successful server initialization, not merely an open database socket.
The package must report `3.3.3`. The tested server manifest digest is
`sha256:bae7664ff3aa4c2c1fcbdcdd86b40b105643c5f861f3e6a770fc760089f25862`,
corresponding to source tag commit
`d2dac658fe1c67e8bbd08aa18200a32e5d45b9e4`.
Do not accept another version or hand-create missing schema objects.

Stop Immich and its queues before synthetic writes; PostgreSQL stays running:

```sh
docker compose -f docker/immich-postgres-test.compose.yml stop server redis
docker compose -f docker/immich-postgres-test.compose.yml exec -T database \
  psql -U postgres -d opm_immich_test_v333 -v ON_ERROR_STOP=1 \
  -c "COMMENT ON DATABASE opm_immich_test_v333 IS 'opm-immich-v3.3.3-disposable'"
printf 'postgresql://postgres:%s@localhost:25432/opm_immich_test_v333?sslmode=verify-full&sslrootcert=%s/tls/ca.crt\n' \
  "$OPM_IMMICH_TEST_PASSWORD" "$OPM_IMMICH_TEST_DIR" \
  > "$OPM_IMMICH_TEST_DIR/database-url"
chmod 600 "$OPM_IMMICH_TEST_DIR/database-url"
export OPM_IMMICH_TEST_DATABASE_URL_FILE="$OPM_IMMICH_TEST_DIR/database-url"
CARGO_BUILD_JOBS=2 cargo test -p opm-server --lib \
  immich::direct_db::postgres_tests::real_v333_postgres_gate \
  -- --ignored --exact
```

The gate reproduces stale-checksum uniqueness failure using the official plain
external-asset insert shape, then proves corrected old-path reuse, unchanged
UUID/album association, exact original/name/sidecar updates, both timestamp and
cursor triggers, idempotence, unchanged content checksum, checksum collisions
before/after preparation, ownership/library/exclusion refusal, stale CAS and
incorrect migration rejection. Interface tests separately cover pre-mutation
refusal, mixed untracked items, post-move outages, crashes, cancellation and retry.

This does **not** establish atomicity against a running Immich scanner.
Discovery/import jobs do not serialize with these transactions. Path-derived
checksum uniqueness reduces one race but is not a scanner lock, especially for
content-hash assets. SQL writes remain unsupported upstream: use current backups,
pause watching and scheduled/manual scans, and drain already queued jobs.

Always clean up this named project, including after failed assertions:

```sh
docker compose -f docker/immich-postgres-test.compose.yml down --volumes
```

Remove only the named test URL and generated TLS files under
`target/immich-postgres-test`. Never use global prune or another project's data.
Only this throwaway gate publishes **localhost:25432**, never production port
5432. Production private-network/TLS/least-privilege guidance is in
[Immich direct database access](README.md#immich-direct-database-access).
Never use the disposable superuser credentials or stack for production.
