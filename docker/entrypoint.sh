#!/bin/sh
# Container entrypoint (runs under tini). It prepares the scratch directory for every command. When the command
# is the server it also checks that the data directory is writable by the uid the container runs as, and stops
# with a message that says how to fix it instead of letting SQLite fail with "unable to open database file".
# Any other command (`docker run IMAGE exiftool -ver`) is passed through untouched.
set -eu

data_dir="${OPM_DATA_DIR:-/data}"
volumes_dir="${OPM_VOLUMES_DIR:-/volumes}"
tmp_dir="${TMPDIR:-/tmp}"

[ "$#" -gt 0 ] || set -- /app/opm-server
case "$1" in
  /app/opm-server | opm-server) is_server=1 ;;
  *) is_server=0 ;;
esac

log() {
  [ "$is_server" = 1 ] || return 0
  printf 'entrypoint: %s\n' "$*" >&2
}

if [ "$is_server" = 1 ]; then
  probe="$data_dir/.opm-write-test.$$"
  if ! touch "$probe" 2>/dev/null; then
    uid="$(id -u)"
    gid="$(id -g)"
    cat >&2 <<EOF
entrypoint: ERROR: the data directory $data_dir is not writable by uid $uid (gid $gid).
The app keeps its database, settings and thumbnail cache there, so it cannot start.
Fix the ownership of the folder mounted at $data_dir on the host, for example over SSH on the NAS:
    sudo chown -R $uid:$gid /volume1/docker/opm
or run the container as the user that owns that folder (compose: user: "UID:GID"; find the numbers with: id <user>).
If the folder is mounted read-only (":ro"), remove that suffix.
On Docker Desktop for Windows, use a Docker named volume for /data and omit the compose "user:" line.
See docker/README.md, section "Users and permissions".
EOF
    exit 1
  fi
  rm -f "$probe"

  # Scratch files left by a killed run would otherwise pile up; only the dedicated directory is ever emptied.
  if [ "$tmp_dir" = "$data_dir/tmp" ]; then
    rm -rf "$tmp_dir" 2>/dev/null || true
  fi

  if [ -z "$(find "$volumes_dir" -mindepth 1 -maxdepth 1 -type d 2>/dev/null | head -n 1)" ]; then
    log "warning: no folders found under $volumes_dir; mount each photo share there, e.g. -v /volume1/photo:$volumes_dir/photo"
  fi
fi

# exiftool, ffmpeg and vips write temp files and caches under TMPDIR and HOME, which must be writable by any uid.
if ! { mkdir -p "$tmp_dir" 2>/dev/null && [ -w "$tmp_dir" ]; }; then
  log "warning: TMPDIR=$tmp_dir is not writable, falling back to /tmp (mount a tmpfs there when the root filesystem is read-only)"
  tmp_dir=/tmp
  export TMPDIR="$tmp_dir"
fi
[ -w "${HOME:-/nonexistent}" ] || export HOME="$tmp_dir"

exec "$@"
