# Install and operate on Synology

For the NAS owner preparing a private, LAN-only deployment. Read the [Docker reference](../docker/README.md)
for environment defaults, mount layout and hardening; the installation commands live in the
[README](../README.md#install-on-synology), not here.

Status: source-verified operations guidance. The current production image was built locally with
`make docker-run`, reached healthy status, and displayed a browsed library from private generated fixtures
with a disposable data folder. No NAS deployment or DSM verification has occurred.
DSM menu names differ between
versions; all DSM UI procedures below are not verified on a DSM device.

## Access safety

1. Keep the published port on a trusted home network. Do not forward it from your router.
   The app has no login: anyone who can reach it can browse and perform permitted file operations.

   You should see: the app is reachable from your intended LAN devices, not from the public internet.

2. If you need remote access, use an authenticated VPN or an authenticating reverse proxy.
   Host checks defend against unrecognised names; the request header and same-origin checks reduce
   cross-site browser requests. They are not user authentication or a firewall.

   You should see: unauthenticated remote users cannot reach the application at all.

## Prepare the NAS

1. Over SSH, identify the architecture and intended filesystem user:

   ```sh
   uname -m
   id DSM_USER
   ```

   Replace `DSM_USER` with the NAS user allowed to access the shares.
   See [supported architectures](../docker/README.md#build); do not attempt a 32-bit image.

   You should see: `x86_64` or `aarch64`, and numeric `uid` and `gid` values to put in Compose.

2. Choose a local NAS folder for `/data`, not an NFS or SMB mount: SQLite WAL requires local storage.
   Create a new app folder and assign just that folder to your chosen user:

   ```sh
   sudo mkdir -p /volume1/docker/opm/data
   sudo chown UID:GID /volume1/docker/opm/data
   ```

   Replace `UID:GID` with the numbers from `id`; change `/volume1` if your local storage differs.
   Do not recursively change ownership of your photo library. Use
   Control Panel > Shared Folder > Edit > Permissions to check the user's share access and ACLs
   (not verified on a DSM device). See [users and permissions](../docker/README.md#users-and-permissions).

   You should see: that user can write app storage and read the intended photo shares.
   Start with a disposable library or read-only archive while checking the setup.

3. Edit the [Compose example](../docker/compose.yml): `user`, the left-hand host paths, published port,
   `TZ` and `OPM_ALLOWED_HOSTS`. Each share mounts at `/volumes/NAME`; its final component is the
   volume name shown in the app. Keep the hardening options.
   A bind mount for `/data` is convenient for NAS backups and inspection, not mandatory:
   a Docker named volume also works when its contents have the selected numeric ownership.

   You should see: each share has its own mount below `/volumes`, and `/data` is writable, not `:ro`.

## Get the image and start the project

1. Follow [Install on Synology](../README.md#install-on-synology) for the primary route: Compose
   validation, pull and start. The image is public, so no registry login or token is needed.

   DSM caveat (not verified on a DSM device): Docker usually needs `sudo`; use the same prefix for
   pull and Compose.

   You should see: Compose pulls the image successfully for your NAS architecture.
   `edge` is a development image; it does not promise the complete application.

2. Alternatively, use the [offline quick start](../docker/README.md#offline-quick-start) and
   [build instructions](../docker/README.md#build): export with `build-image.sh --tar`, transfer
   the tarball, then `docker load`. Set Compose's image to the imported tag.
   Do not rebuild on a small NAS merely to install a published image.

   You should see: the image is available locally before you start the project.

3. In Container Manager > Project > Create, select the folder containing the edited Compose file
   (not verified on a DSM device). If it cannot pull, pre-pull over SSH with `docker compose pull`.
   CLI startup is the alternative already documented in the README.

   You should see: the app service is running. Inspect it from the Compose directory:

   ```sh
   docker compose ps
   docker compose logs --tail=100 opm
   ```

   Use the same `sudo` prefix, if needed, as for `docker compose pull`. Follow the
   [health-check reference](../docker/README.md#health-check) if it is not healthy.

## First start

1. Open the LAN address and **Browse**. The index discovers mounted volumes, reads file metadata
   and linked XMP metadata, hashes file contents and prepares thumbnails in background tasks.
   Browsing reads the index, not the live directory tree. A large first index can take hours:
   hashing must read the contents, not merely list names.

   You should see: mounted volumes followed by indexed folders and items; progress in the top-right
   task indicator. Do not perform destructive actions until your disposable library looks correct.

2. Open **Settings**, **General**, then **Indexing** to adjust worker counts and scanning.
   **Watch for changes** is enabled by default. **Full rescan interval** defaults to 360 minutes;
   0 disables periodic rescanning, while enabled intervals accept 5 through 10080 minutes.
   Keep periodic rescans enabled as a repair path even when watching is healthy.

   You should see: settings changes require **Save**. Worker changes apply to the next scan
   or batch; watcher changes apply live.

3. Understand exclusions before looking for missing files. Defaults skip `@eaDir`, `#recycle`,
   `#snapshot`, `.DS_Store`, `Thumbs.db`, `desktop.ini` and names starting `._`.
   `.opm-trash` and files ending `.opm-part` are always excluded.
   Not every hidden file is ignored. The scanner does not traverse symlinks.
   The app does not index Synology thumbnail/recycle/snapshot folders; `#recycle` and `#snapshot`
   are protected from file-operation cleanup, not treated as expendable junk.

   You should see: your ordinary photo folders, without Synology-generated thumbnail trees.
   Check **Ignored names** and **Ignored prefixes** if an expected name is absent.

## Names and dates

1. Set `OPM_ALLOWED_HOSTS` to the names used by your browser, without `http://` or path components.
   `localhost` and IP literals are always accepted. Ports may be included; see the
   [configuration reference](../docker/README.md#configuration).
   If a DSM reverse proxy exposes `photos.example.com`, allow that hostname and forward the
   browser's original `Host`, including its port, rather than replacing it with an internal name.
   DSM reverse-proxy configuration is not verified on a DSM device.

   You should see: the named URL opens without `host_not_allowed` (403); state-changing requests
   also succeed. `csrf_rejected` behind a proxy can mean `Origin` and forwarded `Host` disagree.

2. Set `TZ` before the first index. Capture dates preserve the photographer's wall-clock time;
   timezone conversion applies to UTC-defined video timestamps and filesystem fallback dates.
   Ordinary embedded EXIF wall-clock dates are not shifted to the NAS timezone.
   Application log timestamps remain UTC even when `TZ` is set.
   If you correct `TZ` later, recreate the container and re-index affected assets.

   You should see: dates without usable camera metadata use the intended local calendar date.
   A cross-volume copy can reset filesystem birth time; keep backups of originals.

## Optional Immich path updates

1. If OPM should update Immich's existing asset when moving a tracked file within a mapped library,
   follow [Immich direct database access](../docker/README.md#immich-direct-database-access).
   The optional `OPM_IMMICH_DATABASE_URL_FILE` setting points to a read-only secret file; it is
   not entered in **Settings**. A DSM-style host path such as
   `/volume1/docker/opm/secrets/immich-database-url` is an example, not verified on a DSM device.

   You should see: **Settings** > **Immich** reports database readiness without showing the secret.

## Watcher and rescan fallback

1. Inspect the read-only status from a trusted client:

   ```sh
   curl --fail http://NAS_ADDRESS:PORT/api/index/status
   ```

   Replace `NAS_ADDRESS` and `PORT` with the reachable LAN address and published port.
   The `watcher.state` is `disabled`, `starting`, `watching` or `degraded`; the response also
   reports watched/unwatched directories, pending directories and whether the limit was reached.

   You should see: `watching` for complete watch coverage, or `degraded` with fallback rescans.

2. On Linux the app registers individual directories with budget
   `min(fs.inotify.max_user_watches / 2, 100000)`. It assumes a system limit of 8192 if that value
   cannot be read. Other NAS apps share the available inotify resources.
   Read the host setting without changing it:

   ```sh
   sysctl fs.inotify.max_user_watches
   ```

   DSM availability and persistence of this setting are not verified on a DSM device.
   Do not blindly raise it: investigate memory and other consumers first.
   Watch intake is bounded; overflow triggers a repair scan. Changed directories settle after
   5 seconds quiet, or 60 seconds of continuous events, and registrations reconcile every 300 seconds.

   You should see: edits eventually appear; periodic rescans repair missed events.
   If coverage is degraded, leave **Full rescan interval** enabled and use **Rescan** after fixing mounts.

## Backups, updates and removal

1. Back up the whole `/data` mount while the app is stopped, plus your photo shares and Compose file.
   `/data/opm.db` contains more than an index: settings, Immich connection/mappings and file/trash
   journals are not disposable. Keep `/data/immich.key` with the database: without it, the encrypted
   API key cannot be recovered after restart. The index and thumbnail cache can be regenerated;
   that does not make the entire database safe to discard. `/data/tmp` is scratch, cleared at startup.
   See [trash recovery](usage.md#trash-and-recovery) before deleting any state.

   You should see: a backup containing database and encryption key from the same stopped state,
   and a share backup that includes `.opm-trash` if you need deleted-file recovery.
   Hyper Backup is an option for your own backup policy, not a recovery mechanism tested here.

2. Follow [Update and tags](../README.md#update-and-tags) after taking that backup.
   Pin an exact published version or `@sha256:MANIFEST_DIGEST` for repeatable deployment;
   replace `MANIFEST_DIGEST` with the image's multiarch manifest digest.
   Version tags and `latest` exist only after the owner approves a release.
   Startup applies migrations; an older binary refuses a database containing newer migrations.
   Do not downgrade against upgraded `/data`; restore the matching app-state backup instead.

   You should see: healthy startup and intact settings. Image replacement does not remove your mounts.

3. Before removing the app, restore anything you want from **Trash** and keep your backups.
   Stopping/removing a container does not remove bind-mounted shares or app storage.
   Per-volume `.opm-trash` directories stay behind. Removing a named data volume destroys the app's
   state; do not do that as routine container cleanup.

   You should see: original shares and any remaining trash still on the NAS after container removal.
