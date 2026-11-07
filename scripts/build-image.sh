#!/usr/bin/env bash
# Builds the Advanced Photo Manager container image from docker/Dockerfile.
#
#   scripts/build-image.sh [--platform LIST] [--tag NAME:TAG]... [--load | --push | --tar DIR] [-- BUILDX_ARGS...]
#
# See docker/README.md for what each mode is for. Compatible with the bash 3.2 that ships with macOS.
set -euo pipefail

DEFAULT_TAG="advanced-photo-manager:dev"
FALLBACK_BUILDER="apm-builder"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

usage() {
  cat <<'EOF'
Usage: scripts/build-image.sh [options] [-- extra "docker buildx build" arguments]

Options:
  --platform LIST   Comma-separated target platforms, e.g. linux/amd64,linux/arm64
                    (default: the platform of the Docker engine, so builds are native and fast)
  --tag NAME:TAG    Image name; may be repeated (default: advanced-photo-manager:dev)
  --load            Load the image into the local Docker engine (default; one platform)
  --push            Push a multi-platform manifest to the registry named in --tag
  --tar DIR         Write one docker-load-able tarball per platform into DIR, for a NAS that cannot
                    reach your registry
  -h, --help        Show this help

Examples:
  scripts/build-image.sh
  scripts/build-image.sh --platform linux/amd64 --tar target/images
  scripts/build-image.sh --platform linux/amd64,linux/arm64 --tag ghcr.io/me/apm:1.0 --push
  scripts/build-image.sh -- --build-arg CARGO_BUILD_JOBS=2 --progress=plain

Set BUILDX_BUILDER to use a specific buildx builder; otherwise a docker-container builder named
apm-builder is created when the active builder cannot do what was asked.
EOF
}

die() {
  echo "build-image: error: $*" >&2
  exit 1
}

info() {
  echo "build-image: $*" >&2
}

platforms=""
tags=()
mode=""
tar_dir=""
extra=()

set_mode() {
  if [ -n "$mode" ] && [ "$mode" != "$1" ]; then
    die "--load, --push and --tar are mutually exclusive"
  fi
  mode="$1"
}

while [ $# -gt 0 ]; do
  case "$1" in
    --platform)
      [ $# -ge 2 ] || die "--platform needs a value"
      platforms="$2"
      shift 2
      ;;
    --platform=*)
      platforms="${1#*=}"
      shift
      ;;
    --tag)
      [ $# -ge 2 ] || die "--tag needs a value"
      tags+=("$2")
      shift 2
      ;;
    --tag=*)
      tags+=("${1#*=}")
      shift
      ;;
    --load)
      set_mode load
      shift
      ;;
    --push)
      set_mode push
      shift
      ;;
    --tar)
      [ $# -ge 2 ] || die "--tar needs a directory"
      set_mode tar
      tar_dir="$2"
      shift 2
      ;;
    --tar=*)
      set_mode tar
      tar_dir="${1#*=}"
      shift
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    --)
      shift
      extra=("$@")
      break
      ;;
    *)
      die "unknown option '$1' (try --help)"
      ;;
  esac
done

[ -n "$mode" ] || mode="load"
[ "${#tags[@]}" -gt 0 ] || tags=("$DEFAULT_TAG")

command -v docker >/dev/null 2>&1 || die "docker is not installed or not on PATH"
docker buildx version >/dev/null 2>&1 || die "docker buildx is required (it ships with Docker Desktop and recent Docker Engine packages)"

daemon_platform="$(docker version --format '{{.Server.Os}}/{{.Server.Arch}}' 2>/dev/null || true)"
[ -n "$daemon_platform" ] || die "cannot reach the Docker daemon; is Docker running?"
[ -n "$platforms" ] || platforms="$daemon_platform"

platform_list=()
old_ifs="$IFS"
IFS=','
for p in $platforms; do
  platform_list+=("$p")
done
IFS="$old_ifs"
for p in "${platform_list[@]}"; do
  case "$p" in
    linux/*) ;;
    *) die "unsupported platform '$p': the image only runs on Linux (expected e.g. linux/amd64 or linux/arm64)" ;;
  esac
done

builder_args=()
builder_driver="$(docker buildx inspect 2>/dev/null | sed -n 's/^Driver:[[:space:]]*//p' | head -n 1)"
containerd_store=false
if docker info --format '{{json .DriverStatus}}' 2>/dev/null | grep -q 'io.containerd.snapshotter'; then
  containerd_store=true
fi

# Without the containerd image store the plain "docker" driver can neither write tarballs nor build several
# platforms at once; those jobs need a docker-container builder.
reason=""
if [ -z "${BUILDX_BUILDER:-}" ] && [ "$builder_driver" = "docker" ] && [ "$containerd_store" = false ]; then
  if [ "$mode" = "tar" ]; then
    reason="tarball output"
  elif [ "${#platform_list[@]}" -gt 1 ]; then
    reason="multi-platform builds"
  fi
fi
if [ -n "$reason" ]; then
  if docker buildx inspect "$FALLBACK_BUILDER" >/dev/null 2>&1; then
    info "the active builder (driver 'docker') cannot do $reason; using the existing '$FALLBACK_BUILDER' builder"
  else
    info "the active builder (driver 'docker') cannot do $reason; creating the docker-container builder '$FALLBACK_BUILDER'"
    info "remove it later with: docker buildx rm $FALLBACK_BUILDER"
    docker buildx create --name "$FALLBACK_BUILDER" --driver docker-container >/dev/null
  fi
  builder_args=(--builder "$FALLBACK_BUILDER")
fi

if [ "$mode" = "load" ] && [ "${#platform_list[@]}" -gt 1 ] && [ "$containerd_store" = false ]; then
  die "--load can only load one platform with this Docker engine; use --tar DIR for one tarball per platform, or --push"
fi

revision=""
version=""
if command -v git >/dev/null 2>&1 && git -C "$ROOT" rev-parse --git-dir >/dev/null 2>&1; then
  revision="$(git -C "$ROOT" rev-parse HEAD 2>/dev/null || true)"
  version="$(git -C "$ROOT" describe --tags --always --dirty 2>/dev/null || true)"
fi
created="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

common_args=(
  --file "$ROOT/docker/Dockerfile"
  --provenance=false
  --label "org.opencontainers.image.created=$created"
)
[ -z "$revision" ] || common_args+=(--label "org.opencontainers.image.revision=$revision")
[ -z "$version" ] || common_args+=(--label "org.opencontainers.image.version=$version")
for t in "${tags[@]}"; do
  common_args+=(--tag "$t")
done

run_build() {
  docker buildx build \
    ${builder_args[@]+"${builder_args[@]}"} \
    "${common_args[@]}" \
    "$@" \
    ${extra[@]+"${extra[@]}"} \
    "$ROOT"
}

case "$mode" in
  load)
    info "building ${tags[0]} for $platforms and loading it into the local Docker engine"
    run_build --platform "$platforms" --load
    info "done: docker run --rm ${tags[0]} ffmpeg -version"
    ;;
  push)
    info "building ${tags[0]} for $platforms and pushing it to its registry"
    run_build --platform "$platforms" --push
    info "pushed: ${tags[*]}"
    ;;
  tar)
    mkdir -p "$tar_dir"
    tar_dir="$(cd "$tar_dir" && pwd)"
    base="$(printf '%s' "${tags[0]}" | tr -c 'A-Za-z0-9._-' '-')"
    for p in "${platform_list[@]}"; do
      file="$tar_dir/$base-$(printf '%s' "$p" | tr '/' '-').tar"
      info "building ${tags[0]} for $p into $file"
      run_build --platform "$p" --output "type=docker,dest=$file"
      info "wrote $file ($(du -h "$file" | cut -f1)); on the NAS: docker load -i $(basename "$file")"
    done
    ;;
esac
