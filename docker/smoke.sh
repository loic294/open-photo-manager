#!/usr/bin/env bash
# End-to-end check of the production image on this machine's architecture (see docker/README.md, "Smoke test").
#
#   docker/smoke.sh                 build the image, check it, clean up
#   SMOKE_SKIP_BUILD=1 docker/smoke.sh   check an image that already exists as $SMOKE_IMAGE (never removed)
#   SMOKE_KEEP_IMAGE=1 docker/smoke.sh   keep the freshly built image afterwards (it is removed by default)
#
# It runs the server as the default user, as an arbitrary uid with a chowned data volume, and with a wrong data
# volume owner, always with a read-only root filesystem and no capabilities (like docker/compose.yml). Everything it
# creates is named apm-smoke-* and is removed on exit. Compatible with the bash 3.2 that ships with macOS.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

IMAGE="${SMOKE_IMAGE:-apm-smoke:local}"
PREFIX="apm-smoke-$$"
WORK="$ROOT/target/docker-smoke/$$"
HOST_UID="$(id -u)"
HOST_GID="$(id -g)"
ARBITRARY_UID=12345
HARDEN=(--read-only --tmpfs /tmp --cap-drop ALL --security-opt no-new-privileges:true)
built_here=0

step() { printf '\n== %s\n' "$*"; }
pass() { printf '   ok: %s\n' "$*"; }

fail() {
  local c
  printf '   FAILED: %s\n' "$*" >&2
  for c in "$PREFIX-a" "$PREFIX-b"; do
    if docker inspect "$c" >/dev/null 2>&1; then
      printf '   --- last log lines of %s\n' "$c" >&2
      docker logs --tail 30 "$c" >&2 || true
    fi
  done
  exit 1
}

cleanup() {
  local status=$?
  trap - EXIT INT TERM
  set +e
  docker rm -f "$PREFIX-a" "$PREFIX-b" "$PREFIX-c" >/dev/null 2>&1
  docker volume rm -f "$PREFIX-data-a" "$PREFIX-data-b" "$PREFIX-data-c" >/dev/null 2>&1
  rm -rf "$WORK"
  if [ "$built_here" = 1 ] && [ -z "${SMOKE_KEEP_IMAGE:-}" ]; then
    docker rmi "$IMAGE" >/dev/null 2>&1
  fi
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

command -v docker >/dev/null 2>&1 || { echo "smoke: docker is not installed" >&2; exit 1; }
command -v curl >/dev/null 2>&1 || { echo "smoke: curl is not installed" >&2; exit 1; }
docker info >/dev/null 2>&1 || { echo "smoke: cannot reach the Docker daemon; is Docker running?" >&2; exit 1; }

http() { curl --noproxy '*' --silent --show-error --max-time 5 "$@"; }

# Creates a small library: an EXIF-tagged JPEG, HEIC, AVIF, an MP4 and accented/space file names.
library_script() {
  cat <<'EOF'
set -eu
cd /volumes
mkdir -p photo/2021 "photo/2022/Été à la plage" archive/old
ffmpeg -v error -y -f lavfi -i testsrc2=size=640x480 -frames:v 1 -update 1 -q:v 3 photo/2021/IMG_0001.jpg
exiftool -q -overwrite_original -DateTimeOriginal='2021:07:04 12:00:00' -Make=Canon -Model='EOS R5' photo/2021/IMG_0001.jpg
vips copy photo/2021/IMG_0001.jpg 'photo/2021/IMG_0002.heic[Q=50]'
vips copy photo/2021/IMG_0001.jpg 'photo/2021/IMG_0003.avif[Q=50]'
cp photo/2021/IMG_0001.jpg "photo/2022/Été à la plage/IMG é 4.jpg"
ffmpeg -v error -y -f lavfi -i testsrc2=size=320x240:rate=15 -t 1 -pix_fmt yuv420p -c:v libx264 "photo/2022/Été à la plage/clip 1.mp4"
cp photo/2021/IMG_0001.jpg archive/old/scan.jpg
EOF
}

# Runs inside a container as whatever uid it has: decodes every format and works in $TMPDIR.
media_script() {
  cat <<'EOF'
set -eu
tmp="${TMPDIR:-/tmp}"
mkdir -p "$tmp"
for f in /volumes/photo/2021/IMG_0001.jpg /volumes/photo/2021/IMG_0002.heic /volumes/photo/2021/IMG_0003.avif; do
  name="$(basename "$f")"
  vipsthumbnail "$f" --size 128 -o "$tmp/t_%s.jpg" 2>/dev/null
  width="$(vipsheader -f width "$tmp/t_${name%.*}.jpg" 2>/dev/null)"
  [ "$width" = 128 ] || { echo "thumbnail of $name is $width px wide, expected 128" >&2; exit 1; }
done
ffmpeg -v error -y -i "/volumes/photo/2022/Été à la plage/clip 1.mp4" -frames:v 1 -update 1 "$tmp/frame.jpg"
[ -s "$tmp/frame.jpg" ] || { echo "no video frame extracted" >&2; exit 1; }
taken="$(exiftool -s3 -DateTimeOriginal -Model /volumes/photo/2021/IMG_0001.jpg | tr '\n' '|')"
[ "$taken" = '2021:07:04 12:00:00|EOS R5|' ] || { echo "unexpected EXIF: $taken" >&2; exit 1; }
accented="$(exiftool -s3 -FileName "/volumes/photo/2022/Été à la plage/IMG é 4.jpg")"
[ "$accented" = 'IMG é 4.jpg' ] || { echo "unexpected accented name: $accented" >&2; exit 1; }
EOF
}

check_tool() {
  local label=$1 out
  shift
  out="$(docker run --rm --pull never "$IMAGE" "$@" 2>&1)" || { printf '%s\n' "$out" >&2; fail "$label does not work in the image"; }
  pass "$label: $(printf '%s\n' "$out" | head -n 1)"
}

wait_healthy() {
  local name=$1 limit=$2 waited=0 state
  while [ "$waited" -lt "$limit" ]; do
    state="$(docker inspect --format '{{.State.Status}} {{if .State.Health}}{{.State.Health.Status}}{{else}}none{{end}}' "$name" 2>/dev/null || echo 'gone none')"
    case "$state" in
      'running healthy') pass "Docker reports $name healthy after about ${waited}s"; return 0 ;;
      'running starting') ;;
      *) fail "$name is '$state' instead of healthy" ;;
    esac
    sleep 2
    waited=$((waited + 2))
  done
  fail "$name did not become healthy within ${limit}s"
}

published_port() {
  docker port "$1" "$2/tcp" | head -n 1 | sed 's/.*://'
}

http_checks() {
  local base=$1 body page result
  body="$(http --fail "$base/api/health")" || fail "GET /api/health failed"
  printf '%s' "$body" | grep -Eq '"status"[[:space:]]*:[[:space:]]*"ok"' || fail "unexpected /api/health body: $body"
  pass "GET /api/health -> $body"
  page="$(http --fail "$base/")" || fail "GET / failed"
  printf '%s' "$page" | grep -Eqi '<!doctype html|<html' || fail "GET / did not return the web app"
  pass "GET / serves the web app"
  result="$(http --output /dev/null --write-out '%{http_code} %{content_type}' "$base/api/does-not-exist")" || fail "GET /api/does-not-exist failed"
  case "$result" in
    '404 application/json'*) pass "unknown /api path -> $result" ;;
    *) fail "unknown /api path returned '$result' instead of a JSON 404" ;;
  esac
}

exec_checks() {
  local name=$1 uid=$2
  [ "$(docker exec "$name" id -u)" = "$uid" ] || fail "$name does not run as uid $uid"
  [ "$(docker exec "$name" cat /proc/1/comm)" = tini ] || fail "PID 1 of $name is not tini"
  docker exec "$name" sh -c 'touch /data/.smoke-write && rm /data/.smoke-write' || fail "/data is not writable as uid $uid"
  docker exec "$name" sh -c 'touch /tmp/.smoke-write && rm /tmp/.smoke-write' || fail "/tmp (tmpfs) is not writable as uid $uid"
  [ "$(docker exec "$name" stat -c %u /data/tmp)" = "$uid" ] || fail "/data/tmp is not owned by uid $uid"
  if docker exec --user 0:0 "$name" sh -c 'touch /app/.smoke-write' >/dev/null 2>&1; then
    fail "the root filesystem of $name is writable"
  fi
  if docker exec "$name" sh -c 'touch /volumes/archive/.smoke-write' >/dev/null 2>&1; then
    fail "the read-only archive mount of $name is writable"
  fi
  pass "uid $uid, tini is PID 1, /data and /tmp writable, root filesystem and :ro mount are not"
}

media_checks() {
  local out
  out="$(docker exec "$1" sh -ec "$(media_script)" 2>&1)" || { printf '%s\n' "$out" >&2; fail "media tools failed inside $1"; }
  pass "thumbnails of JPEG/HEIC/AVIF, video frame, EXIF and accented names work in $1"
}

logs_clean() {
  local logs
  logs="$(docker logs "$1" 2>&1)"
  case "$logs" in
    *panicked*) fail "the log of $1 contains a panic" ;;
  esac
}

stop_checks() {
  local name=$1 started ended code
  started="$(date +%s)"
  docker stop --time 10 "$name" >/dev/null
  ended="$(date +%s)"
  code="$(docker inspect --format '{{.State.ExitCode}}' "$name")"
  [ "$code" = 0 ] || fail "$name exited with code $code after docker stop (expected a clean 0)"
  [ $((ended - started)) -le 5 ] || fail "docker stop took $((ended - started))s (expected a quick graceful shutdown)"
  pass "docker stop shut $name down cleanly in $((ended - started))s"
}

mkdir -p "$WORK/volumes"

if [ -z "${SMOKE_SKIP_BUILD:-}" ]; then
  step "building $IMAGE for this machine's platform"
  "$ROOT/scripts/build-image.sh" --tag "$IMAGE"
  built_here=1
else
  docker image inspect "$IMAGE" >/dev/null 2>&1 || { echo "smoke: SMOKE_SKIP_BUILD is set but image $IMAGE does not exist" >&2; exit 1; }
fi

step "image metadata"
user="$(docker image inspect --format '{{.Config.User}}' "$IMAGE")"
case "$user" in
  '' | 0 | 0:* | root | root:*) fail "the image runs as root (USER '$user')" ;;
  *) pass "default user is $user" ;;
esac
health="$(docker image inspect --format '{{json .Config.Healthcheck}}' "$IMAGE")"
case "$health" in
  *'"Interval":30000000000'*'"StartPeriod":20000000000'*) pass "health check: every 30s, 20s start period" ;;
  *) fail "unexpected health check: $health" ;;
esac
entrypoint="$(docker image inspect --format '{{json .Config.Entrypoint}}' "$IMAGE")"
case "$entrypoint" in
  *'/usr/bin/tini'*) ;;
  *) fail "tini is not the entrypoint: $entrypoint" ;;
esac
exposed="$(docker image inspect --format '{{json .Config.ExposedPorts}}' "$IMAGE")"
case "$exposed" in
  *'8080/tcp'*) ;;
  *) fail "port 8080 is not exposed: $exposed" ;;
esac
pass "tini entrypoint, port 8080 exposed"
arch="$(docker image inspect --format '{{.Architecture}}' "$IMAGE")"
# Every later docker command runs the image's own platform. This also silences Docker's "platform does not match"
# warning when an image built for another architecture is checked under emulation (SMOKE_SKIP_BUILD=1).
export DOCKER_DEFAULT_PLATFORM="linux/$arch"
size_mb="$(docker run --rm --pull never --user 0:0 "$IMAGE" du -sxm / 2>/dev/null | cut -f1 || true)"
pass "architecture $arch, about ${size_mb:-?} MB unpacked"

step "tools inside the image"
check_tool "ffmpeg" ffmpeg -version
check_tool "ffprobe" ffprobe -version
check_tool "exiftool" exiftool -ver
check_tool "vipsthumbnail" vipsthumbnail --vips-version
check_tool "tini" tini --version
check_tool "curl" curl --version
check_tool "web UI" ls /app/web/index.html
check_tool "non-root default user" sh -c 'test "$(id -u)" -ne 0 && id'
setuid_files="$(docker run --rm --pull never "$IMAGE" find / -xdev -type f '(' -perm -4000 -o -perm -2000 ')' 2>/dev/null || true)"
[ -z "$setuid_files" ] || fail "setuid/setgid files in the image: $setuid_files"
pass "no setuid/setgid files"

step "sample library"
docker run --rm --pull never --user "$HOST_UID:$HOST_GID" --tmpfs /tmp -e HOME=/tmp -e TMPDIR=/tmp \
  -v "$WORK/volumes:/volumes" --entrypoint sh "$IMAGE" -ec "$(library_script)" || fail "could not create the sample library"
pass "created JPEG, HEIC, AVIF, MP4 and accented names under $WORK/volumes"

step "A: default user (uid 1000), read-only root, no capabilities, :ro archive mount"
docker run -d --pull never --name "$PREFIX-a" "${HARDEN[@]}" -p 127.0.0.1::8080 \
  -v "$PREFIX-data-a:/data" \
  -v "$WORK/volumes/photo:/volumes/photo" -v "$WORK/volumes/archive:/volumes/archive:ro" \
  "$IMAGE" >/dev/null
wait_healthy "$PREFIX-a" 120
port_a="$(published_port "$PREFIX-a" 8080)"
http_checks "http://127.0.0.1:$port_a"
exec_checks "$PREFIX-a" 1000
if docker exec -e APM_LISTEN=127.0.0.1:9 "$PREFIX-a" /app/healthcheck.sh >/dev/null 2>&1; then
  fail "the health check succeeds although nothing listens on the configured port"
fi
pass "the health check fails when nothing listens on the configured port"
media_checks "$PREFIX-a"
logs_clean "$PREFIX-a"
stop_checks "$PREFIX-a"

step "B: arbitrary uid $ARBITRARY_UID, custom port, data volume owned by that uid"
# A named volume stands in for the NAS folder because macOS bind mounts do not enforce ownership. Docker re-applies
# the image's /data ownership (1000:1000) to a volume that is still empty when a container is created, so the volume
# gets a seed file first, like a NAS folder that already holds data (a bind mount has no such copy-up).
docker run --rm --pull never --user 0:0 --entrypoint sh -v "$PREFIX-data-b:/data" "$IMAGE" -ec \
  "chown $ARBITRARY_UID:$ARBITRARY_UID /data && touch /data/.seed && chown $ARBITRARY_UID:$ARBITRARY_UID /data/.seed" \
  || fail "could not hand the data volume to uid $ARBITRARY_UID"
docker run -d --pull never --name "$PREFIX-b" --user "$ARBITRARY_UID:$ARBITRARY_UID" "${HARDEN[@]}" \
  -e APM_LISTEN=0.0.0.0:9090 -p 127.0.0.1::9090 \
  --health-interval 2s --health-start-period 0s \
  -v "$PREFIX-data-b:/data" \
  -v "$WORK/volumes/photo:/volumes/photo" -v "$WORK/volumes/archive:/volumes/archive:ro" \
  "$IMAGE" >/dev/null
wait_healthy "$PREFIX-b" 60
port_b="$(published_port "$PREFIX-b" 9090)"
http_checks "http://127.0.0.1:$port_b"
exec_checks "$PREFIX-b" "$ARBITRARY_UID"
docker exec "$PREFIX-b" sh -c 'echo persisted > /data/.smoke-marker' || fail "could not write to /data as uid $ARBITRARY_UID"
media_checks "$PREFIX-b"
logs_clean "$PREFIX-b"
stop_checks "$PREFIX-b"
owner="$(docker run --rm --pull never --entrypoint stat -v "$PREFIX-data-b:/data" "$IMAGE" -c '%u:%g' /data/.smoke-marker)"
[ "$owner" = "$ARBITRARY_UID:$ARBITRARY_UID" ] || fail "the file written to /data is owned by $owner"
pass "files written to /data belong to $owner and survive the container"

step "C: data volume not writable by the container's uid"
set +e
refusal="$(docker run --rm --pull never --name "$PREFIX-c" --user "$ARBITRARY_UID:$ARBITRARY_UID" "${HARDEN[@]}" \
  -v "$PREFIX-data-c:/data" "$IMAGE" 2>&1)"
code=$?
set -e
[ "$code" -ne 0 ] || fail "the server started although /data is not writable"
case "$refusal" in
  *'not writable'*'chown'*) pass "refused to start (exit $code) with a message that explains how to fix the ownership" ;;
  *) printf '%s\n' "$refusal" >&2; fail "the refusal message does not explain the problem" ;;
esac

printf '\nSMOKE OK: %s (%s, about %s MB unpacked) in %ss\n' "$IMAGE" "$arch" "${size_mb:-?}" "$SECONDS"
