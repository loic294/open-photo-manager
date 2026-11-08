# Explicit Immich v3.3.3 PostgreSQL gate

This is an **isolated integration setup**, not production configuration. It runs the
real v3.3.3 server's Kysely migrations, with its release-pinned PostgreSQL/vector
and Valkey images. No synthetic replacement schema is accepted. The adapter
requires the exact migration list ending in
`1790693088454-AddPersonUserTableSharedBySharedWithConstraint`, actual enum values
`sha1`/`sha1-path`, the checksum uniqueness index and enabled cursor triggers.

Do not point this gate at an existing Immich installation. It writes synthetic
assets and intentionally reproduces a checksum collision. Its database name and
explicit disposable marker are mandatory safety guards. Ordinary Rust tests do
not require Docker; the command below is **explicitly required before landing**
this feature, and fails when its secret file, PostgreSQL or schema is absent.
An ignored test in ordinary `cargo test` is not a successful integration gate.

Check Docker VM writable capacity first. Uncached Immich/PostgreSQL images and
their layers need substantially more room than PostgreSQL data alone. Do not
prune shared images, containers, volumes or build caches to make space. Use only
this Compose project's volumes and remove them afterwards.

From this isolated checkout, after ensuring localhost port 25432 is free:

```sh
export APM_IMMICH_TEST_DIR="$PWD/target/immich-postgres-test"
export APM_IMMICH_TEST_PASSWORD="$(openssl rand -hex 24)"
mkdir -p "$APM_IMMICH_TEST_DIR/tls"
openssl req -x509 -newkey rsa:2048 -nodes -days 1 \
  -keyout "$APM_IMMICH_TEST_DIR/tls/server.key" \
  -out "$APM_IMMICH_TEST_DIR/tls/server.crt" \
  -subj '/CN=localhost' -addext 'subjectAltName=DNS:localhost'
chmod 600 "$APM_IMMICH_TEST_DIR/tls/server.key"
docker compose -f docker/immich-postgres-test.compose.yml up -d --wait
docker compose -f docker/immich-postgres-test.compose.yml logs --no-color server
```

Wait for the server to report successful initialization, **not just an open
PostgreSQL socket**. Verify the immutable source identity from inside the image:
its `/usr/src/app/package.json` must report `3.3.3`; record the resolved server
image digest with the gate result. The verified source tag corresponds to commit
`d2dac658fe1c67e8bbd08aa18200a32e5d45b9e4`. Do not accept another version
or hand-create missing tables, functions, extensions or migrations.

Stop Immich and its queues before the write tests; PostgreSQL stays running:

```sh
docker compose -f docker/immich-postgres-test.compose.yml stop server redis
docker compose -f docker/immich-postgres-test.compose.yml exec -T database \
  psql -U postgres -d apm_immich_test_v333 -v ON_ERROR_STOP=1 \
  -c "COMMENT ON DATABASE apm_immich_test_v333 IS 'apm-immich-v3.3.3-disposable'"
printf '%s\n' "postgres://postgres:$APM_IMMICH_TEST_PASSWORD@localhost:25432/apm_immich_test_v333?sslmode=verify-full&sslrootcert=$APM_IMMICH_TEST_DIR/tls/server.crt" \
  > "$APM_IMMICH_TEST_DIR/database-url"
chmod 600 "$APM_IMMICH_TEST_DIR/database-url"
export APM_IMMICH_TEST_DATABASE_URL_FILE="$APM_IMMICH_TEST_DIR/database-url"
CARGO_BUILD_JOBS=2 cargo test -p apm-server --lib \
  immich::direct_db::postgres_tests::real_v333_postgres_gate \
  -- --ignored --exact
```

The test asserts the stale-digest uniqueness failure using the official plain
external-asset insert shape, corrected old-path reuse, the moved asset's unchanged
UUID/album association, exact original/name/sidecar updates, changed trigger
cursors, idempotence, content checksum preservation, destination-checksum
collision refusal, stale CAS and incorrect migration rejection. Interface tests
separately exercise filesystem refusal, post-move outages and durable retries.

This does **not** establish atomicity against a running Immich scanner. Its file
discovery and import jobs are not synchronized with this writer's transactions.
The path-derived unique checksum reduces one race but is not a scanner lock,
especially for content-hash assets. Direct SQL writes remain unsupported by
upstream; a production operator needs a current backup and must pause watching,
scheduled/manual scans and drain already queued jobs.

Always clean up this named project, including after a failing assertion:

```sh
docker compose -f docker/immich-postgres-test.compose.yml down --volumes
```

Remove only the named `target/immich-postgres-test/database-url` and TLS files
created above. Never use global Docker prune or delete another project's data.
The Compose file publishes only **localhost:25432 for this throwaway gate**, not
host port 5432. Production configuration and least-privilege considerations are
in [Docker image: Immich direct database access](README.md#immich-direct-database-access).
Never use the throwaway
superuser role/password or this test Compose stack for production.
