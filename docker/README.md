# Docker image

See also: [Synology operations](../docs/synology.md), [Immich](../docs/immich.md),
[usage and recovery](../docs/usage.md), [troubleshooting](../docs/troubleshooting.md).

Advanced Photo Manager ships as one image: the Rust server, the built web app, and the media tools it needs
(`ffmpeg`, `exiftool`, `libvips` with HEIC/AVIF). It runs as a non-root user, works with a read-only root
filesystem and has a built-in health check. It is meant for a Synology NAS but runs on any Docker host.

> The app has **no login** in v1. Run it on your home network only. Do not publish its port to the internet; if you
> need remote access, put it behind a VPN or a reverse proxy that authenticates users.

| File | Purpose |
|---|---|
| `Dockerfile` | Multi-stage build: web app (Node), server (Rust, dependency layer cached with cargo-chef), slim Debian runtime |
| `entrypoint.sh` | Runs under `tini`; checks that `/data` is writable and prepares the scratch folder, then starts the server |
| `healthcheck.sh` | Docker `HEALTHCHECK`: `GET /api/health` on the port the server listens on |
| `compose.yml` | Example for Synology Container Manager (copy it to the NAS and edit the `CHANGE ME` lines) |
| `check_compose.py` | Checks the root README's embedded Compose example; `make check-compose` also validates Compose |
| `docker.mk` | `make docker-build`, `make docker-run`, `make docker-smoke` (included by the root `Makefile`) |
| `smoke.sh` | End-to-end check of a freshly built image |
| `../scripts/build-image.sh` | Builds for one or several architectures; loads, pushes or exports the image |
| `../.dockerignore` | Allowlist of what is sent to the builder (sources only, no `target/`, `node_modules/` or secrets) |

## Immich direct database access

Tracked in-library moves require **exactly Immich v3.3.3**. The API still handles
offline-confirmed asset deletion. Direct SQL writes are not officially supported
by Immich: keep a current backup, pause watching and scheduled/manual scans, and
drain queued scan jobs. A PostgreSQL transaction does **not** serialize with those
jobs; an active scan can still race, import a duplicate or remove the old asset.
This contract is derived from the immutable
[v3.3.3 source](https://github.com/immich-app/immich/tree/d2dac658fe1c67e8bbd08aa18200a32e5d45b9e4)
and its [database-write warning](https://github.com/immich-app/immich/blob/d2dac658fe1c67e8bbd08aa18200a32e5d45b9e4/docs/docs/guides/database-queries.md),
not an officially supported move API.

Create a separate Compose override rather than publishing PostgreSQL's port.
The example below joins APM to the existing Immich database's private Docker
network; replace its name and the host-side secret paths to match your setup:

```yaml
services:
  apm:
    environment:
      APM_IMMICH_DATABASE_URL_FILE: /run/secrets/immich-postgres-url
    volumes:
      - /volume1/docker/apm/secrets/immich-postgres-url:/run/secrets/immich-postgres-url:ro
      - /volume1/docker/apm/secrets/immich-postgres-ca:/run/secrets/immich-postgres-ca:ro
    networks:
      - default
      - immich_database
networks:
  immich_database:
    external: true
    name: immich_backend
```

The URL file contains a single PostgreSQL URL. Use the database's **private DNS
name**, which must match its TLS certificate, explicit `sslmode=verify-full` and
`sslrootcert=/run/secrets/immich-postgres-ca`. URL-encode credentials and restrict
host-file permissions to the APM container's numeric uid/gid. Never place the URL
in the browser, a settings field, source control or logs. There is no plaintext
database-password UI and no direct URL environment variable; only the dedicated
adapter reads this read-only file. Restart APM after changing its configuration.
Never publish host port 5432 or grant access to unrelated Docker networks.

A database administrator should provision a dedicated role, not reuse Immich's
superuser. It needs SELECT on `public.kysely_migrations`, `asset`, `asset_file`
and `library`; UPDATE on `asset`'s `originalPath`, `originalFileName`, `checksum`
and `asset_file.path`; and execution of the existing cursor-trigger functions and
their dependencies. PostgreSQL's `FOR SHARE` library lock additionally requires
UPDATE privilege on at least one library column: an administrator can choose
`name` as that limited grant; APM does not issue a library UPDATE. Do not grant
DDL, deletion, schema ownership, superuser, trigger-disabling or role-management
permissions. Test the grants against a backed-up disposable copy first.

Settings reports the last executor readiness reason without exposing the secret.
Every tracked move checks API version, exact migration state, schema, ownership,
library boundaries, supported exclusions and destination collisions before
replacing, transferring or changing sidecars. A failed preflight refuses that
item with **"No file was moved"** and recovery instructions; unrelated untracked
items continue. A database failure after movement remains durably queued with
an **unconfirmed path update** warning. Finish/retry it before moving that file
again or reusing its old source path. Avoid changing the connection/mappings
while a move or its path update is in flight. APM's tracked classification uses its synchronized Immich index: refresh
sync before moving; API-invisible locked assets are not discovered by that index.

The supported exclusion subset is the six v3.3.3 defaults (`**/@eaDir/**`,
`**/._*`, `**/#recycle/**`, `**/#snapshot/**`, `**/.stversions/**`,
`**/.stfolder/**`) plus `**/.apm-trash/**`, with case-insensitive matching.
Unsupported custom globs fail closed with a specific warning. In-place moves
cannot cross libraries, change `libraryId`, or enter excluded/out-of-library
paths. Moves outside mapped libraries and trash keep the existing API deletion
checks. Linked sidecar updates use each asset's actual copied/moved destination;
a shared source sidecar may intentionally remain.

For stored checksum algorithm `sha1-path`, the same transaction updates the
digest using Immich's official `sha1("path:" + path.posix.normalize(newPath))`;
this allows later imports at the vacated source path. Stored algorithm `sha1`
(upstream `sha1File`) remains byte-for-byte unchanged. Unknown algorithms and
destination path/checksum ownership collisions are refused. UUIDs/associations
and enabled timestamp/updateId triggers are preserved.

Before changing this adapter, run the mandatory isolated real-schema gate in
[immich-postgres-test.md](immich-postgres-test.md). An ignored test is not a
successful PostgreSQL gate.

## Public GHCR publishing

The normal installation path is [the root README](../README.md), using the public image
`ghcr.io/loic294/open-photo-manager:edge`. Pulling needs no registry login or token.

[`.github/workflows/publish-image.yml`](../.github/workflows/publish-image.yml) publishes `edge` on `main`
pushes affecting image inputs (not docs-only changes), or on manual dispatch from `main`. Approved exact
`vX.Y.Z` tags publish `X.Y.Z`, `X.Y` and `latest`; no tag or release is created by CI. GitHub ignores path
filters for tag pushes. Manual dispatch from other branches is rejected.

The workflow runs only in `loic294/open-photo-manager`, uses only `GITHUB_TOKEN` with job-scoped package
permissions, and labels images with the repository source URL. Provenance and SBOM attestations are disabled.
The source label links the package to this public repository, and the first run published it public. The last job
step pulls the first published tag without credentials and warns, without failing the run, when GHCR does not
answer 200. Fix such a warning in the package settings (Danger Zone > Change visibility): GitHub offers no
workflow or token route for package visibility.

Each architecture builds natively on `ubuntu-24.04` (amd64) or `ubuntu-24.04-arm` (arm64), pushes an untagged
digest, pulls it and runs `docker/smoke.sh`. Only after both pass are tags assembled and inspected for exactly
those two Linux digests. Failed runs can leave untagged digests, but do not promote them. All actions are pinned
to full commit SHAs with version comments. Do not silently substitute emulation if a native runner is unavailable.

The repository owner deliberately creates `vX.Y.Z` release tags after verifying a build; the workflow
never creates tags itself. Development publishing is enabled immediately; `latest` is reserved for releases
after final application verification.
`make check` includes `make check-compose` (Python 3 and Docker Compose required).

## Offline quick start

1. Build the image on your computer for the NAS architecture and export it as a file (see [Build](#build)):
   ```sh
   scripts/build-image.sh --platform linux/amd64 --tag advanced-photo-manager:latest --tar target/images
   ```
2. Copy `target/images/advanced-photo-manager-latest-linux-amd64.tar` and `docker/compose.yml` to the NAS, then load
   the image over SSH: `docker load -i advanced-photo-manager-latest-linux-amd64.tar`. In the DSM interface use
   Container Manager > Image > Action > Import > Add from File instead.
3. Create the data folder and give it to the user the app will run as
   (see [Users and permissions](#users-and-permissions)):
   ```sh
   sudo mkdir -p /volume1/docker/apm/data
   sudo chown -R 1026:100 /volume1/docker/apm/data
   ```
4. Edit `compose.yml`: replace the GHCR `image:` with `advanced-photo-manager:latest` (the tag loaded above),
   and set user, photo folders, `TZ`, `APM_ALLOWED_HOSTS`. Start it:
   `docker compose up -d`, or Container Manager > Project > Create and pick the folder holding the file.
5. Open `http://<nas-name-or-ip>:8080`.

## Build

```sh
make docker-build                                    # this machine's architecture, tag advanced-photo-manager:dev
make docker-build APM_IMAGE=advanced-photo-manager:latest
scripts/build-image.sh --help
```

`make docker-build` calls `scripts/build-image.sh`. The script options:

| Option | Meaning |
|---|---|
| `--platform LIST` | Comma separated platforms, e.g. `linux/amd64,linux/arm64`. Default: the platform of your Docker engine, so the build is native and fast. |
| `--tag NAME:TAG` | Image name, repeatable. Default `advanced-photo-manager:dev`. For offline use, set Compose's `image:` to the tag you imported. |
| `--load` | Load the image into the local Docker engine (default, one platform). |
| `--push` | Push a multi-platform manifest to the registry named in `--tag`. |
| `--tar DIR` | Write one tarball per platform into `DIR`, named `<name>-<tag>-linux-<arch>.tar`. For a NAS that cannot reach your registry. |
| `-- ARGS` | Everything after `--` is passed to `docker buildx build`, e.g. `-- --build-arg CARGO_BUILD_JOBS=2`. |

Which architecture does the NAS need? Run `uname -m` on it: `x86_64` means `linux/amd64`, `aarch64` means
`linux/arm64`. Many Synology models are `x86_64`, some entry-level ones are ARM. 32-bit ARM models (`armv7l`) are not
supported.

```sh
# A file to copy to the NAS (no registry needed). To shrink the copy: gzip -k file.tar (docker load reads .tar.gz too)
scripts/build-image.sh --platform linux/amd64 --tag advanced-photo-manager:latest --tar target/images
# One manifest with both architectures in a registry
scripts/build-image.sh --platform linux/amd64,linux/arm64 --tag ghcr.io/you/advanced-photo-manager:1.0 --push
```

Things to know:

- **Emulation is slow.** Building for another architecture than the one of your machine (for example `linux/amd64` on
  an Apple Silicon Mac) runs the Rust compiler under emulation. It works but can take an hour or more. For regular
  releases build on a machine of the target architecture, or use one native CI runner per platform and `--push`.
  On a Linux host without Docker Desktop, install the emulators once with
  `docker run --privileged --rm tonistiigi/binfmt --install all`.
- **Builders.** With Docker Desktop's containerd image store (Settings > General) every mode works with the default
  builder. Without it the plain `docker` driver cannot build several platforms or write tarballs, so the script
  creates a `docker-container` builder named `apm-builder` and says so (remove it with `docker buildx rm apm-builder`).
  Set `BUILDX_BUILDER` to use a builder of your own.
- **Memory.** The Rust compile can use several GB of RAM when many cores run in parallel. On a small machine or VM
  pass `-- --build-arg CARGO_BUILD_JOBS=2`.
- **Caching.** Dependencies are compiled in their own layer (cargo-chef) and the npm install is cached, so after a
  source change only the application crates and the web bundle are rebuilt (a few seconds to a minute).
- **Old Docker engines.** Images are built without provenance attestations, which keeps them plain images that
  `docker load` and older Docker engines (such as the ones shipped with DSM) handle.
- Toolchain versions are build arguments of the `Dockerfile` (`RUST_VERSION`, `NODE_VERSION`, `DEBIAN_RELEASE`).
  `RUST_VERSION` must match `rust-toolchain.toml`; the build stops if they differ.

## Run

Compose (recommended): see `compose.yml`, its comments explain every line.

The same thing with `docker run`:

```sh
docker run -d --name apm --restart unless-stopped \
  --user 1026:100 \
  --read-only --tmpfs /tmp --cap-drop ALL --security-opt no-new-privileges:true \
  -p 8080:8080 \
  -e TZ=Europe/Paris -e APM_ALLOWED_HOSTS=diskstation,diskstation.local \
  -v /volume1/photo:/volumes/photo \
  -v /volume1/docker/apm/data:/data \
  advanced-photo-manager:latest
```

On your own computer, `make docker-run` builds the image and runs it as your user with a scratch data folder
(`target/docker-run/data`), the generated test library (`fixtures/out/volumes`) when it exists, and the hardening
flags above. Open `http://localhost:8080` (`make docker-run APM_PORT=9000` for another port).

## Configuration

Set these as environment variables. An empty value counts as not set.

| Variable | Default in the image | Meaning |
|---|---|---|
| `APM_LISTEN` | `0.0.0.0:8080` | Address and port the server listens on inside the container. If you change the port, change the published port too. The health check follows this value. |
| `APM_VOLUMES_DIR` | `/volumes` | Folder whose direct sub-folders are the volumes. |
| `APM_DATA_DIR` | `/data` | Where the app keeps its own files. Must be writable. |
| `APM_WEB_DIR` | `/app/web` | The built web app. Leave it alone. |
| `APM_LOG` | `info` | Log filter: `error`, `warn`, `info`, `debug`, or finer such as `info,apm_server::indexer=debug`. Logs go to stdout (`docker logs apm`). |
| `APM_WATCH_MODE` | `auto` | `auto` selects watchers by mounted filesystem type, `watch` forces them on, and `poll` disables watchers globally. It does not change the full-rescan interval. |
| `APM_ALLOWED_HOSTS` | (empty) | Names you type in the browser to reach the app, comma separated: `diskstation`, `diskstation:8080`, `*.example.com`. `localhost` and IP addresses always work. **Set this when you use a name**, otherwise the app answers 403 "host not allowed". |
| `APM_IMMICH_QUIET_SECS` | `15` | Seconds of quiet file operations before queued changes are mirrored to Immich; accepts 1 through 600. An invalid value refuses startup. This is not an import-completion deadline. |
| `APM_IMMICH_DATABASE_URL_FILE` | (unset) | Read-only secret file containing the PostgreSQL URL for tracked in-library moves, Immich v3.3.3 only. See [Immich direct database access](#immich-direct-database-access) for required TLS, grants, backup and scanner precautions. Never use a Settings field or a plaintext URL environment variable. |
| `APM_INDEX_CONCURRENCY` | (empty: the smaller of 2 and the CPU count) | How many files are analysed at once. Use `1` on a NAS with little memory. |
| `TZ` | `UTC` | Time zone name, e.g. `Europe/Paris`. Used for filesystem fallback dates and UTC-defined video dates. Embedded wall-clock dates are preserved; application log timestamps remain UTC. |
| `LANG` | `C.UTF-8` | Keeps accented file names intact in the media tools. Leave it alone. |
| `TMPDIR`, `HOME` | `/data/tmp` | Scratch space for `ffmpeg`, `exiftool` and `vips`. See below. |

## Volumes and files

| Container path | What it is | Writable |
|---|---|---|
| `/volumes/<name>` | One mount per photo share. Every folder directly below `/volumes` is one volume in the app, and `<name>` is the name it shows. Add as many as you like. | Yes, unless you add `:ro` |
| `/data` | The app's own files: database, thumbnail cache, settings, secrets. Back this folder up. | Must be |
| `/data/tmp` | Scratch folder for the media tools. The app empties it every time it starts, so never keep anything there. | Created automatically |
| `/tmp` | Plain temporary space. A tmpfs is enough (`tmpfs: [/tmp]`). | Yes |
| `/app` | The server and the web app. | No (part of the image) |

A read-only mount (`-v /volume1/archive:/volumes/archive:ro`) lets you browse and search a share but makes it
impossible for the app to change or delete anything there.

## Users and permissions

The container never runs as root. The `user: "UID:GID"` line of the compose file chooses who the app is, and that
decides what it may read, create, move and delete in your shares. It is ordinary Linux file permission checking,
and Synology ACLs on top of it. Any numbers work, they do not have to exist inside the image. When you set no
`user:`, the container runs as the built-in `apm` user (uid and gid 1000).

1. Pick a DSM user that has read and write permission on the photo shared folders (Control Panel > Shared Folder >
   Edit > Permissions), or a dedicated one.
2. Find its numbers over SSH: `id <user>` prints something like `uid=1026(admin) gid=100(users)`.
   Put them in the compose file: `user: "1026:100"`.
3. Give that user the data folder:
   `sudo mkdir -p /volume1/docker/apm/data && sudo chown -R 1026:100 /volume1/docker/apm/data`.

Use a folder on the NAS (a bind mount) for `/data`, as `compose.yml` does, for convenient backup and inspection.
A Docker named volume also works, including with an arbitrary non-root UID, when it has the correct
ownership for the selected `user:`. A newly populated volume inherits the image directory's ownership
(1000:1000); prepare ownership for another UID before running the app. Do not mistake an ownership failure
for a requirement to run as root or to abandon named volumes.

If `/data` is not writable by the user, the container stops at once and says so in its log (`docker logs apm`)
instead of failing later with a database error.

On Docker Desktop for Windows with Linux containers, use a Docker named volume for `/data` and omit `user:`.
Docker Desktop's user mapping does not use the NAS's numeric uid/gid convention. Windows photo folders can be
bind-mounted under `/volumes`; use the volume details in Settings to see whether watching and case sensitivity
are available on the mounted filesystem. SQLite WAL is required for `/data`: startup stops with an actionable
error if the data mount is known to be unsafe or the live WAL probe fails. Keep `/data` on a local Docker named
volume rather than a Windows drive bind mount.

### Watch policy

`APM_WATCH_MODE=auto` is the default: known network/shared filesystems that do not deliver host-side change
events are polled by the configured full-rescan interval. Set `watch` to force filesystem watchers on every
online volume, or `poll` to disable watchers globally. Prefer `auto` unless a filesystem's behavior is known;
the setting does not change the rescan interval.

## Hardening

Everything below is on in `compose.yml` and in `make docker-run`, and the smoke test runs with it:

- **Non-root, no setuid files.** The image has no setuid or setgid binary and runs as the user you choose.
- **`read_only: true` with `tmpfs: [/tmp]`.** The container cannot modify its own filesystem. The only writable
  places are `/data`, the photo mounts you left read-write, and the `/tmp` tmpfs.
- **`cap_drop: [ALL]` and `no-new-privileges`.** No Linux capabilities, no way to gain more.
- **`tini` as PID 1** forwards signals and reaps child processes (`ffmpeg`, `exiftool`), so `docker stop` ends the
  server cleanly within a second or two.
- **No secrets in the image.** Nothing is baked in; keys the app creates live in `/data`.

## Health check

`healthcheck.sh` asks `http://127.0.0.1:<port>/api/health` (the port comes from `APM_LISTEN`) every 30 seconds,
with a 20 second start period and 3 retries. See the result with
`docker inspect --format '{{.State.Health.Status}}' apm` or in `docker ps` (`healthy`). The container is `unhealthy`
when the server stops answering; check `docker logs apm`.

## Smoke test

```sh
make docker-smoke
```

Builds the image for this machine's architecture (cached builds take seconds) and then checks, with a read-only
root filesystem, no capabilities and a read-only archive mount:

- `ffmpeg`, `ffprobe`, `exiftool`, `vipsthumbnail`, `tini` and `curl` run; the user is not root; the web app is
  there; there are no setuid files.
- With the default user and with an arbitrary uid (12345) on a data volume owned by that uid: the container
  becomes `healthy`, `/api/health` answers, `/` serves the web app, unknown `/api/*` paths return JSON (not the
  web app), thumbnails of JPEG, HEIC and AVIF files, video frames, EXIF reading and accented file names all work,
  `/data` is writable and `docker stop` is clean and fast.
- With a data volume the user cannot write to, the container refuses to start and the message explains the fix.

It takes about half a minute with a warm cache and removes everything it creates (names start with `apm-smoke-`).
`SMOKE_SKIP_BUILD=1 SMOKE_IMAGE=name:tag` checks an image that already exists instead of building one, and
`SMOKE_KEEP_IMAGE=1` keeps the image it built.

## What is inside, and how big it is

- `debian:bookworm-slim` with `ffmpeg` 5.1 (video frames and probing), `libimage-exiftool-perl` 12.57 (metadata and
  the previews embedded in RAW files), `libvips-tools` 8.14 with HEIC and AVIF (thumbnails), `curl` (health check),
  `tini`, `tzdata` and `ca-certificates` (HTTPS connections, for example to Immich).
- About 460 MB unpacked (`docker run --rm IMAGE du -sxm /`) and about 165 MB compressed, which is what a pull
  transfers. `docker images` can show more (around 650 MB) because the containerd image store adds both numbers.
- That is more than a plain Rust service because Debian's `ffmpeg` and `libvips` pull in about 340 packages
  (codecs, Perl for `exiftool`, ICU, speech synthesis libraries linked into `ffmpeg`). The Dockerfile already
  removes the biggest parts nothing here uses: Mesa/LLVM/Z3 (software OpenGL pulled in by `ffmpeg`'s display
  devices) and Python. A check in the same layer fails the build if a tool then loses a shared library.
- **Do not run `apt` or `dpkg` inside the image.** That removal uses `dpkg --purge --force-depends`, which leaves
  the package database inconsistent on purpose. Change the Dockerfile instead.

## Troubleshooting

| Symptom | Cause and fix |
|---|---|
| The browser shows "host not allowed" (403) | You opened the app with a name the server does not know. Add it to `APM_ALLOWED_HOSTS` (`diskstation`, `diskstation.local`, ...) and restart. |
| Container exits at once; log says the data directory is not writable | The user in `user:` does not own the `/data` folder. `sudo chown -R UID:GID <data folder>` (see [Users and permissions](#users-and-permissions)). With a read-only `:ro` mount on `/data` remove the suffix. |
| `unhealthy` | `docker logs apm`. If you changed `APM_LISTEN`, the health check follows it, but the published port must match. |
| The app cannot see my photos | Each share must be mounted directly under `/volumes` (for example `/volumes/photo`), not at `/volumes` itself. The log warns when `/volumes` has no folders. |
| Folders look empty, or changes are refused | The `user:` has no permission on the share. Check the DSM permissions for that user. |
| HEIC/AVIF photos have no thumbnails | `docker run --rm IMAGE vips --vips-config` must say `HEIC/AVIF load/save with libheif: true`. If it does not, rebuild the image with `make docker-build`. |
| The NAS runs out of memory while indexing | Set `APM_INDEX_CONCURRENCY: 1` and, in `compose.yml`, enable `mem_limit`. |
| Photo fallback dates are off by hours | Set `TZ` to your time zone and re-index affected files. Application logs use UTC even when `TZ` is set. |
| `read-only file system` errors in the log | Keep `tmpfs: [/tmp]` next to `read_only: true`. |
| `docker load` says the file is not an image, or the image will not start | The tarball was built for another architecture. Check `uname -m` on the NAS and rebuild with the matching `--platform`. |
| An image build is killed or the machine freezes | Not enough memory for the Rust compile: `-- --build-arg CARGO_BUILD_JOBS=2`, or give Docker more RAM. |
| The build says a file or folder is missing although it exists in the repository | `.dockerignore` only lets sources through. When the build starts to need a new top-level path (for example a file pulled in with `include_str!`), add `!that/path` there and, if the runtime needs it, a `COPY` line in `docker/Dockerfile`. |
| The build stops with "rust-toolchain.toml pins a different compiler" | The compiler pinned in `rust-toolchain.toml` and `ARG RUST_VERSION` in `docker/Dockerfile` must match: update the Dockerfile when the pin changes. |

Detailed Synology guidance (shared folder permissions and ACLs, the same share in Immich, `@eaDir` and `#recycle`
folders, inotify limits, backups) is covered in the documentation that follows this one.
