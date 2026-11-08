# Docker Desktop on Windows 11

This guide covers Docker Desktop running Linux containers on Windows 11, commonly through WSL 2.
The app has not been tested on a Windows machine. Its real-mount probe has only been exercised
inside a Linux container on a Docker-for-Mac bind mount; Windows-drive behavior described here is
based on the implementation and the linked platform guidance, not a Windows acceptance test.

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
volume. Replace the sample path with an existing folder and use the private-image access steps in
the [GHCR login steps in the README](../README.md#install-on-synology); this is separate from the
Synology Compose example.

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

## Photo folders and filesystem behavior

Windows-drive photo folders can be bind-mounted under `/volumes`, as in the example. For WSL 2,
Docker's [best-practices guide](https://docs.docker.com/desktop/features/wsl/best-practices/)
recommends storing bind-mounted data in the Linux filesystem for performance; it also notes that
Linux containers receive inotify change events when the source files are in that Linux filesystem.
This is general Docker guidance, not a claim that APM has verified Windows-drive speed or event
delivery.

`APM_WATCH_MODE` is `auto` by default. In `auto`, APM chooses watchers by the detected filesystem
type and polls known polling-only types; unknown types retain native watcher behavior. `watch`
forces watchers on, while `poll` disables them globally. Neither the setting nor forced watching
changes the full-rescan interval or guarantees that a Windows-drive mount emits usable events. Review
the watcher status and volume details in the app; the host warning codes are `volumes_polling_only`
(external changes wait for the next full rescan, or use Re-index if rescans are off) and
`unreliable_volume_inodes` (some file operations may fail safely). See the
[Docker reference](../docker/README.md#watch-policy).

The app probes mounted volumes for filesystem type, case sensitivity and inode reliability, but
those probes do not establish that every Windows filesystem operation is supported. Behavior and
performance on Windows drives, 9P speed, OneDrive, Defender interaction, Windows path-length limits
and Windows network shares have not been verified on a Windows machine. Do not treat an observed
capability value or a passing probe on another host as a Windows guarantee.
