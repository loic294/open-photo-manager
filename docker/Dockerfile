# Advanced Photo Manager: production image (Rust server + built web UI + the media tools it shells out to).
#
# Build from the repository root with scripts/build-image.sh (or `docker buildx build -f docker/Dockerfile .`).
# Running, configuration and troubleshooting: docker/README.md.

ARG RUST_VERSION=1.94.1
ARG NODE_VERSION=24
ARG DEBIAN_RELEASE=bookworm
ARG CARGO_CHEF_VERSION=0.1.78

# ---- web: the static UI ------------------------------------------------------------------------------------
# The output is plain HTML/JS/CSS, so this stage always runs on the build machine's own architecture
# ($BUILDPLATFORM) and never needs emulation, whatever platform the image is built for.
FROM --platform=$BUILDPLATFORM node:${NODE_VERSION}-${DEBIAN_RELEASE}-slim AS web
ENV NPM_CONFIG_UPDATE_NOTIFIER=false
WORKDIR /src/web
COPY web/package.json web/package-lock.json web/.npmrc ./
RUN --mount=type=cache,target=/root/.npm \
    npm ci --no-audit --no-fund
COPY web/ ./
RUN npm run build

# ---- chef: Rust toolchain + cargo-chef ---------------------------------------------------------------------
# The Rust stages run natively on the target platform (no cross-compilation). The image already contains the
# pinned compiler; rust-toolchain.toml is never copied into a build stage and RUSTUP_TOOLCHAIN names the
# installed version, so rustup has nothing to download.
FROM rust:${RUST_VERSION}-${DEBIAN_RELEASE} AS chef
ARG RUST_VERSION
ARG CARGO_CHEF_VERSION
ENV RUSTUP_TOOLCHAIN=${RUST_VERSION}
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    cargo install --locked --version "${CARGO_CHEF_VERSION}" cargo-chef
WORKDIR /src

# ---- planner: dependency recipe (depends on manifests only, so source edits keep the cooked layer) ----------
FROM chef AS planner
RUN --mount=type=bind,source=rust-toolchain.toml,target=/pin/rust-toolchain.toml \
    grep -Eq "^channel[[:space:]]*=[[:space:]]*\"${RUSTUP_TOOLCHAIN}\"" /pin/rust-toolchain.toml \
    || { echo "rust-toolchain.toml pins a different compiler than RUST_VERSION=${RUSTUP_TOOLCHAIN}: update docker/Dockerfile." >&2; exit 1; }
COPY Cargo.toml Cargo.lock ./
COPY .cargo .cargo
COPY crates crates
RUN cargo chef prepare --recipe-path /recipe.json

# ---- builder: dependencies first (cached), then the server ------------------------------------------------
# Low-memory machines (Docker Desktop VMs, small NAS): build with --build-arg CARGO_BUILD_JOBS=2.
FROM chef AS builder
ARG CARGO_BUILD_JOBS
COPY --from=planner /recipe.json /recipe.json
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    cargo chef cook --release --locked --recipe-path /recipe.json -p apm-server
COPY Cargo.toml Cargo.lock ./
COPY .cargo .cargo
COPY crates crates
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    cargo build --release --locked -p apm-server \
 && install -D -m 0755 target/release/apm-server /out/apm-server

# ---- runtime ---------------------------------------------------------------------------------------------------
FROM debian:${DEBIAN_RELEASE}-slim AS runtime

# ffmpeg: video thumbnails; exiftool: metadata; vips: image thumbnails (HEIC/AVIF through libheif);
# curl: HEALTHCHECK; tini: PID 1; tzdata: TZ; ca-certificates: TLS to Immich.
#
# Debian's ffmpeg (through SDL) and libvips-tools (through a Python helper script) also pull in Mesa's software
# OpenGL drivers with LLVM and Z3, and Python: about 185 MB that nothing here uses (the drivers are only loaded
# when an OpenGL context is created). They are purged in the same layer so the space is really saved, which
# leaves dpkg's dependency bookkeeping inconsistent: do not run apt in this image. The loop makes the build fail
# if any shipped program or libvips module needs a library that is gone.
RUN set -eu; \
    apt-get update; \
    DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        ca-certificates curl ffmpeg libimage-exiftool-perl libvips-tools tini tzdata; \
    rm -rf /var/lib/apt/lists/*; \
    unused="$(dpkg-query -W -f '${db:Status-Abbrev}|${Package}\n' libgl1-mesa-dri 'libllvm*' 'libz3-*' 'python3*' 'libpython3*' | sed -n 's/^ii *|//p')"; \
    if [ -n "$unused" ]; then dpkg --purge --force-depends $unused; fi; \
    for f in /usr/bin/ffmpeg /usr/bin/ffprobe /usr/bin/vips /usr/bin/vipsthumbnail /usr/bin/curl /usr/bin/perl /usr/bin/tini /usr/lib/*/vips-modules-*/*.so; do \
        [ -e "$f" ] || { echo "missing $f" >&2; exit 1; }; \
        if ldd "$f" 2>&1 | grep -q 'not found'; then echo "unresolved libraries in $f" >&2; ldd "$f" >&2; exit 1; fi; \
    done

# uid/gid 1000 is only the default: the container is meant to run as whatever user owns the shared folders
# (compose `user: "UID:GID"`). /data is owned by 1000 so a fresh named volume is writable by the default user;
# bind mounts keep the host's ownership. Setuid/setgid bits are dropped (nothing here needs them).
RUN groupadd --gid 1000 apm \
 && useradd --uid 1000 --gid 1000 --no-create-home --home-dir /nonexistent --shell /usr/sbin/nologin apm \
 && install -d -o 1000 -g 1000 -m 0755 /data \
 && install -d -m 0755 /volumes /app \
 && find / -xdev -type f \( -perm -4000 -o -perm -2000 \) -exec chmod a-s {} +

COPY --from=builder /out/apm-server /app/apm-server
COPY --from=web /src/web/dist /app/web
COPY --chmod=0755 docker/entrypoint.sh docker/healthcheck.sh /app/
COPY LICENSE /app/LICENSE

# The image documents its own configuration contract; see docker/README.md for each variable.
# Empty values mean "unset" to the server. TMPDIR/HOME live on the /data volume so any uid can write them and
# temp files can be renamed into /data/cache atomically (same filesystem); the entrypoint empties it on start.
ENV APM_LISTEN=0.0.0.0:8080 \
    APM_VOLUMES_DIR=/volumes \
    APM_DATA_DIR=/data \
    APM_WEB_DIR=/app/web \
    APM_LOG=info \
    APM_ALLOWED_HOSTS="" \
    APM_INDEX_CONCURRENCY="" \
    TZ=UTC \
    LANG=C.UTF-8 \
    TMPDIR=/data/tmp \
    HOME=/data/tmp

LABEL org.opencontainers.image.title="Advanced Photo Manager" \
      org.opencontainers.image.description="Browse, de-duplicate and organise a photo library on a NAS." \
      org.opencontainers.image.licenses="MIT"

USER 1000:1000
WORKDIR /app
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 CMD ["/app/healthcheck.sh"]
ENTRYPOINT ["/usr/bin/tini", "--", "/app/entrypoint.sh"]
CMD ["/app/apm-server"]
