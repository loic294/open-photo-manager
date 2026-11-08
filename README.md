# Advanced Photo Manager

Browse, filter and organise a photo library on a Synology NAS. One Docker image contains the Rust server,
web interface and media tools, with native Linux amd64 and arm64 builds.

Five tools compare duplicates, move assets, format folders, clean up empty folders and batch-rename
files. Filter and select files, inspect them in the viewer, index metadata and `.xmp` sidecars in the
background, and connect an Immich external library.

Private repository and private image: `loic294/advanced-photo-manager` and
`ghcr.io/loic294/advanced-photo-manager`. The app has no login in v1: keep it on your trusted LAN,
do not forward port 8080 or expose it publicly. Back up your photos and the app's data folder.

## Before you start

- Host, Origin and request-header checks reduce browser cross-site attacks; they do not authenticate
  anyone on your LAN. Never publish the app's port. [Access safety](docs/synology.md#access-safety).
- File deletion sends content to `<volume>/.apm-trash`. Emptying trash permanently removes it;
  enabling automatic retention also permanently removes old trash. Retention is off by default.
  [Trash and recovery](docs/usage.md#trash-and-recovery).
- The first index reads file contents to hash them and extracts metadata, including linked `.xmp`
  sidecars. Later scans process changes; allow for hours on a large library because hashing reads
  every file. [First start](docs/synology.md#first-start).
- Set `TZ` before indexing and `APM_ALLOWED_HOSTS` before opening the app by name.
  [Names and dates](docs/synology.md#names-and-dates).
- Immich must see the same share through an external library. Mirroring is asynchronous and
  metadata carry-over has limits. [Immich guide](docs/immich.md).
- Still images such as HEIC, TIFF and RAW get JPEG viewer renditions. Video is streamed unchanged;
  a thumbnail does not mean the browser supports its codec.
  [Media troubleshooting](docs/troubleshooting.md#browsing-and-media).

## Installation guide

1. Identify your NAS architecture and choose a numeric user.
2. Prepare local app storage and photo-share permissions.
3. Follow the existing private GHCR installation below.
4. Check container health, then browse a disposable test library first.
5. Follow the [Synology operations guide](docs/synology.md) for checks, backups and DSM caveats.

## Install on Synology

Use DSM Container Manager and a 64-bit Intel/AMD or ARM NAS (`uname -m`: `x86_64` or `aarch64`).
The commands below run over SSH on the NAS; use an account permitted to run Docker.

1. Create `/volume1/docker/apm`, save the Compose example below as `compose.yml` there, and edit its
   `CHANGE ME` settings. Use `id <dsm-user>` for the numeric UID/GID of a user allowed to access your
   photo shares. Create `/volume1/docker/apm/data` and give that user ownership:
   ```sh
   sudo mkdir -p /volume1/docker/apm/data
   sudo chown 1026:100 /volume1/docker/apm/data
   cd /volume1/docker/apm
   ```
   On DSM, Docker commands usually need `sudo`; this has not been verified on a DSM device.
   Docker login is per OS user, so run `docker login`, `docker compose pull` and `docker compose up -d`
   consistently with `sudo`, or consistently without it.
2. Get a classic GitHub personal access token with `read:packages`, belonging to an account granted
   read access to the private package (and authorize SSO if required). Log in interactively:
   ```sh
   docker login ghcr.io --username YOUR_GITHUB_USERNAME
   ```
   Enter the token at Docker's password prompt yourself. Never put it in Compose, shell command arguments,
   source control or messages to an agent. Docker stores login credentials on the NAS; protect that account
   and its Docker configuration, or use a credential helper. `GITHUB_TOKEN` is for CI, not NAS installation.
3. Pull and start:
   ```sh
   docker compose config -q
   docker compose pull
   docker compose up -d
   docker compose ps
   ```
   Alternatively create a Container Manager Project from this directory after logging in and pulling.
   Open `http://<nas-ip>:8080`; for a hostname, include it in `APM_ALLOWED_HOSTS`.

## Update and tags

From `/volume1/docker/apm`, run `docker compose pull && docker compose up -d`. Existing bind-mounted photos
and `/data` remain in place. Back up `/data` before an update; check `docker compose logs --tail=100 apm`
if the image's built-in health check does not become healthy.

- `edge`: development image from eligible pushes to `main`; can include unfinished features.
- `X.Y.Z`: versioned image from an explicitly approved `vX.Y.Z` Git tag.
- `X.Y` and `latest`: move with approved version tags, not development pushes. They do not exist until
  the first approved version is published.
- For a repeatable deployment, replace `:edge` with an exact version or `@sha256:<manifest-digest>`.
  Docker selects the NAS architecture from the multiarch manifest.

The repository owner deliberately creates `vX.Y.Z` release tags after verifying a build; the workflow
never creates tags itself. No release tag is created by the publishing workflow. Registry-free installation
from a locally built tarball remains available in [docker/README.md](docker/README.md).

## Compose example

This is the verbatim [docker/compose.yml](docker/compose.yml). Run `make check-compose` after editing
either copy. Keep the published port on your LAN; the private registry does not authenticate the app.

<!-- BEGIN COMPOSE -->
```yaml
# Advanced Photo Manager: example Docker Compose file for a Synology NAS (also fine on any Docker host).
#
# How to use it (details in README.md; offline builds in docker/README.md):
#   1. Log in to ghcr.io with a classic PAT with read:packages, then `docker compose pull`.
#   2. Copy this file to the NAS, for example /volume1/docker/apm/compose.yml, and edit the lines marked CHANGE ME.
#   3. Create the data folder and give it to the user in "user:" below (see "Users and permissions" in the README):
#        sudo mkdir -p /volume1/docker/apm/data && sudo chown -R 1026:100 /volume1/docker/apm/data
#   4. Start it: `docker compose up -d`, or Container Manager > Project > Create (pick the folder with this file).
#   5. Open http://<nas-ip>:8080

services:
  apm:
    # Private development image; pin a version or digest for repeatable deployments.
    image: ghcr.io/loic294/advanced-photo-manager:edge
    # Alternatively let Compose build it from a checkout of the repository (slow on a NAS, building on a PC is easier):
    # build:
    #   context: ..
    #   dockerfile: docker/Dockerfile
    container_name: apm
    restart: unless-stopped

    # CHANGE ME: the numeric "uid:gid" the app runs as. It decides which files the app may read and change.
    # Use a DSM user that can read and write your photo folders. Find the numbers over SSH with `id <dsm-user>`.
    # On Synology the first administrator is usually 1026 and the "users" group is 100.
    user: "1026:100"

    ports:
      # "host-port:container-port". Change the left number if 8080 is already used on your NAS.
      - "8080:8080"

    volumes:
      # Every folder mounted directly below /volumes in the container becomes one volume in the app.
      # The last part of the container path is the volume name shown in the app.
      # CHANGE ME: the left side is the folder on your NAS.
      - /volume1/photo:/volumes/photo
      # A read-only volume: the app can browse it but can never change or delete anything in it.
      # - /volume1/archive:/volumes/archive:ro
      # The app's own files (database, thumbnails, secret key). It must be writable by the user above.
      - /volume1/docker/apm/data:/data

    environment:
      # CHANGE ME: your time zone, e.g. Europe/Paris or America/New_York. Used for log times and for photos without
      # a time zone of their own.
      TZ: Europe/Paris
      # Names (not IP addresses) you type in the browser to reach the app. Without this line the app answers
      # "host not allowed" when you use a name. Comma separated, e.g. "diskstation,diskstation.local,photos.example.com".
      APM_ALLOWED_HOSTS: "diskstation,diskstation.local"
      # How much the app logs (error, warn, info, debug).
      # APM_LOG: info
      # How many photos are analysed at the same time. Lower it to 1 on a NAS with little memory.
      # APM_INDEX_CONCURRENCY: 2

    # Hardening: nothing inside the container can change the image, gain privileges or use special capabilities.
    read_only: true
    tmpfs:
      - /tmp
    cap_drop:
      - ALL
    security_opt:
      - no-new-privileges:true

    # Gives the app time to finish what it is doing when you stop the container (the app waits up to 10 seconds).
    stop_grace_period: 20s

    # Keeps the log files from growing without limit.
    logging:
      driver: json-file
      options:
        max-size: "10m"
        max-file: "3"

    # Optional: protects the rest of the NAS if a huge video or photo is analysed. Remove the "#" to enable it.
    # mem_limit: 1g

    # The health check ("is the app answering?") is part of the image, nothing to add here.
```
<!-- END COMPOSE -->

## Documentation

These guides describe the repository implementation. A published development image may lag behind it.

| Guide | For |
|---|---|
| [Synology](docs/synology.md) | Installation checks, permissions, indexing, updates and backups |
| [Immich](docs/immich.md) | Connection, external-library mappings and safe asynchronous mirroring |
| [Usage](docs/usage.md) | Browsing, duplicate matching, moving, formatting, batch rename, cleanup, settings and trash |
| [Windows hosts](docs/windows.md) | Docker Desktop, SQLite data storage, mount behavior and unverified Windows-specific limitations |
| [Troubleshooting](docs/troubleshooting.md) | Symptoms, error codes and local diagnostics |
| [Docker reference](docker/README.md) | Image builds, environment variables, mounts and hardening |

## Development

Run `make fixtures` to generate a test library; `make dev-server` and `make dev-web` start the backend
and frontend. From a worktree use your own ports, not the shared 8080/5173 defaults:

```sh
APM_LISTEN=127.0.0.1:SERVER_PORT make dev-server
APM_API_TARGET=http://127.0.0.1:SERVER_PORT npm --prefix web run dev -- --port WEB_PORT --strictPort
```

Replace `SERVER_PORT` and `WEB_PORT` with unused numeric ports in both commands. Run them in separate
terminals. Destructive checks use a copy of the fixture volumes and a separate `APM_DATA_DIR`.

`make check` covers Rust, web and Compose; `make check-docs` covers documentation contracts.
`make check-tools` checks the separate developer tools. `make mock-immich` starts the shared test
service; [its reference](tools/mock-immich/README.md) explains scenarios and isolated mounts.
`make docker-smoke` builds and exercises the production image; it is not a DSM test.

| Path | Contents |
|---|---|
| `crates/apm-core/` | Pure domain logic and API DTOs |
| `crates/apm-server/` | HTTP server, SQLite, indexer, file operations and Immich |
| `crates/apm-server/migrations/` | Append-only database migrations |
| `crates/apm-server/tests/` | Integration tests |
| `web/` | Lit and DaisyUI application |
| `web/src/api/generated/` | TypeScript DTOs exported from Rust |
| `docker/` | Production image and deployment reference |
| `fixtures/` | Disposable test-library generator |
| `tools/` | Developer tools, including mock Immich |
| `docs/` | User guides and documentation checks |

Contributor rules are in [AGENTS.md](AGENTS.md) and [web/AGENTS.md](web/AGENTS.md).
