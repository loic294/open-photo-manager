# Docker Desktop on Windows 11

This guide covers Docker Desktop running Linux containers on Windows 11, commonly through WSL 2.
The app has not been tested on a Windows machine. Its real-mount probe has only been exercised
inside a Linux container on a Docker-for-Mac bind mount; Windows-drive behavior described here is
based on the implementation and the linked platform guidance, not a Windows acceptance test.

For Windows users running Docker Desktop with Linux containers and the WSL 2 backend.

## Install

1. Install and start Docker Desktop, select Linux containers, and enable its WSL 2 backend.
   Windows-specific setup and behavior have not been verified on a Windows machine.

   You should see: Docker Desktop running Linux containers and able to run `docker compose` commands.

2. Log in to the private GitHub Container Registry image. Follow only the
   [README's GHCR login steps](../README.md#install-on-synology); enter the token interactively
   at Docker's password prompt. Do not put the token in the Compose file or a command argument.

   You should see: Docker reports `Login Succeeded`.

3. Save the Compose example below as `compose.yml` in a new folder. Replace the sample photo path
   with an existing Windows photo folder. Keep the named `apm-data` volume mounted at `/data`.

   You should see: `compose.yml` has the Windows photo folder at `/volumes/photos` and `apm-data`
   at `/data`.

4. From that folder, start the app:

   ```powershell
   docker compose up -d
   ```

   You should see: `docker compose ps` reports the `apm` service running.

5. Check the health endpoint from PowerShell:

   ```powershell
   curl.exe http://localhost:8080/api/health
   ```

   You should see: JSON with `"status":"ok"`, a server `"version"`, and a `"warnings"` array (which
   may be empty or contain host warnings).

6. Open [http://localhost:8080](http://localhost:8080).

   You should see: the APM web interface. Browse a disposable test folder before organizing real
   photos.

## Keep app data on a local Docker volume

Mount `/data` from a local Docker named volume, not from a Windows drive or a network share. APM uses
SQLite in WAL mode only. Startup refuses to continue when the mounted filesystem is known to be
unsafe for WAL or when the live WAL probe fails. There is no rollback-journal fallback or degraded
mode. See [SQLite's network-filesystem cautions](https://www.sqlite.org/useovernet.html): remote
filesystem locking and sync behavior can be unreliable, and an early successful test is not a
guarantee of safety.

The named volume persists independently of the photo folders. Include it in your backup plan. If
moving an existing `/data` directory, stop the app and back it up before migrating its contents; do
not discard the original data directory until the new setup is verified.

## Compose example

This standalone example bind-mounts a Windows photo folder and keeps SQLite data in a Docker named
volume. Replace the sample path with an existing folder. The login step above links to the README's
private GHCR instructions; this Windows Compose example is separate from the Synology Compose example.

```yaml
services:
  apm:
    image: ghcr.io/loic294/advanced-photo-manager:edge
    restart: unless-stopped
    ports:
      - "8080:8080"
    volumes:
      - type: bind
        source: "C:/Users/YOUR_NAME/Pictures"
        target: /volumes/photos
        bind:
          create_host_path: false
      - type: volume
        source: apm-data
        target: /data
    environment:
      # Choose the host time zone when dates fall back to file times or UTC-defined video timestamps.
      # Embedded EXIF wall-clock capture times are preserved and are not converted. See the Docker reference.
      TZ: UTC
      APM_WATCH_MODE: auto
    read_only: true
    tmpfs:
      - /tmp
    cap_drop:
      - ALL
    security_opt:
      - no-new-privileges:true
    stop_grace_period: 20s

volumes:
  apm-data:
```

There is deliberately no `user:` entry: Docker Desktop's Linux-container user mapping differs
from the numeric UID/GID setup used on a NAS, and the image runs as its built-in non-root user by
default. `localhost` and IP addresses are accepted by default; add any browser hostname to
`APM_ALLOWED_HOSTS`. Keep the published port on a trusted LAN because the app has no user login.
`TZ` controls filesystem fallback dates and UTC-defined video dates; ordinary embedded EXIF
wall-clock dates are not shifted to this time zone. See the
[Docker reference's Compose time-zone comment](../docker/compose.yml) and
[date guidance](synology.md#names-and-dates) when choosing a value.

## Photo folders and filesystem behavior

Windows-drive photo folders can be bind-mounted under `/volumes`, as in the example. For WSL 2,
Docker's [best-practices guide](https://docs.docker.com/desktop/features/wsl/best-practices/)
recommends storing bind-mounted data in the Linux filesystem for performance; it also notes that
Linux containers receive inotify change events when the source files are in that Linux filesystem.
This is general Docker guidance, not a claim that APM has verified Windows-drive speed or event
delivery.

When APM detects a case-insensitive mounted volume, **Settings** > **General** > **Volumes** shows
the **case-insensitive** badge. It can also show the detected filesystem type, **polling only**,
**read-only** and **offline** status badges as applicable. These are probe/status details, not a
Windows support guarantee; the app preserves each mounted filesystem's name behavior.

`APM_WATCH_MODE` is `auto` by default. In `auto`, APM chooses watchers by the detected filesystem
type and polls known polling-only types; unknown types retain native watcher behavior. `watch`
forces watchers on, while `poll` disables them globally. Neither the setting nor forced watching
changes the full-rescan interval or guarantees that a Windows-drive mount emits usable events. Review
the watcher status and volume details in the app; the host warning codes are `volumes_polling_only`
(external changes wait for the next full rescan, or use Re-index if rescans are off) and
`unreliable_volume_inodes` (some file operations may fail safely). See the
[Docker reference](../docker/README.md#watch-policy).

## Immich connectivity

Immich must be reachable from the APM container. If both services are attached to a shared Docker
network, use Immich's service name and container port; separate Compose projects do not share a
network automatically. Docker Desktop documents `host.docker.internal` for reaching a service on
the host ([networking guide](https://docs.docker.com/desktop/features/networking/)), but that route
is general Docker Desktop guidance and has not been verified on a Windows machine. See the
[Immich guide](immich.md#prerequisites-and-shared-paths) for external library path mappings.

## Restart and scope

The example's `restart: unless-stopped` asks Docker to restart the container after it exits. Whether
Docker Desktop itself starts automatically at Windows sign-in, and whether that brings this service
back as expected, have not been verified on a Windows machine.

Windows containers and a native Windows build of APM are out of scope; this guide is for the Linux
container image only.

The app probes mounted volumes for filesystem type, case sensitivity and inode reliability, but
those probes do not establish that every Windows filesystem operation is supported. Behavior and
performance on Windows drives, 9P speed, OneDrive, Defender interaction, Windows path-length limits
and Windows network shares have not been verified on a Windows machine. Do not treat an observed
capability value or a passing probe on another host as a Windows guarantee.
