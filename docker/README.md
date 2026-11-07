# Docker image

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
| `docker.mk` | `make docker-build`, `make docker-run`, `make docker-smoke` (included by the root `Makefile`) |
| `smoke.sh` | End-to-end check of a freshly built image |
| `../scripts/build-image.sh` | Builds for one or several architectures; loads, pushes or exports the image |
| `../.dockerignore` | Allowlist of what is sent to the builder (sources only, no `target/`, `node_modules/` or secrets) |

## Quick start

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
4. Edit `compose.yml` (user, photo folders, `TZ`, `APM_ALLOWED_HOSTS`) and start it:
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
| `--tag NAME:TAG` | Image name, repeatable. Default `advanced-photo-manager:dev`. `compose.yml` expects `advanced-photo-manager:latest`. |
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
| `APM_ALLOWED_HOSTS` | (empty) | Names you type in the browser to reach the app, comma separated: `diskstation`, `diskstation:8080`, `*.example.com`. `localhost` and IP addresses always work. **Set this when you use a name**, otherwise the app answers 403 "host not allowed". |
| `APM_INDEX_CONCURRENCY` | (empty: the smaller of 2 and the CPU count) | How many files are analysed at once. Use `1` on a NAS with little memory. |
| `TZ` | `UTC` | Time zone name, e.g. `Europe/Paris`. Used for log times and for photos that carry no time zone. |
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

Use a folder on the NAS (a bind mount) for `/data`, as `compose.yml` does. A Docker named volume works only
when the container runs as the default user (leave out `user:`): Docker hands a new, empty named volume the
ownership of the image's `/data` (1000:1000) and does so again whenever the volume is still empty at container
creation, so a `chown` of an empty named volume does not last.

If `/data` is not writable by the user, the container stops at once and says so in its log (`docker logs apm`)
instead of failing later with a database error.

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
| Log times or photo dates are off by hours | Set `TZ` to your time zone. |
| `read-only file system` errors in the log | Keep `tmpfs: [/tmp]` next to `read_only: true`. |
| `docker load` says the file is not an image, or the image will not start | The tarball was built for another architecture. Check `uname -m` on the NAS and rebuild with the matching `--platform`. |
| An image build is killed or the machine freezes | Not enough memory for the Rust compile: `-- --build-arg CARGO_BUILD_JOBS=2`, or give Docker more RAM. |
| The build says a file or folder is missing although it exists in the repository | `.dockerignore` only lets sources through. When the build starts to need a new top-level path (for example a file pulled in with `include_str!`), add `!that/path` there and, if the runtime needs it, a `COPY` line in `docker/Dockerfile`. |
| The build stops with "rust-toolchain.toml pins a different compiler" | The compiler pinned in `rust-toolchain.toml` and `ARG RUST_VERSION` in `docker/Dockerfile` must match: update the Dockerfile when the pin changes. |

Detailed Synology guidance (shared folder permissions and ACLs, the same share in Immich, `@eaDir` and `#recycle`
folders, inotify limits, backups) is covered in the documentation that follows this one.
