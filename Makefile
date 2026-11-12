# Open Photo Manager: development shortcuts (see AGENTS.md).

# Defaults for `make dev-server`; override from the environment or the make command line.
OPM_LOG ?= debug
OPM_VOLUMES_DIR ?= fixtures/out/volumes
OPM_DATA_DIR ?= .data
OPM_WEB_DIR ?= web/dist

.DEFAULT_GOAL := check

.PHONY: build check check-rust check-web clean dev-server dev-web fmt gen-api

check: check-rust check-web

check-rust:
	cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace

check-web:
	test -d web/node_modules || npm --prefix web ci
	npm --prefix web run check

fmt:
	cargo fmt --all && npm --prefix web run format

# Regenerates every TypeScript type from opm-core::dto (stale files are removed first).
gen-api:
	rm -f web/src/api/generated/*.ts
	cargo test -p opm-core export_bindings

dev-server:
	OPM_LOG=$(OPM_LOG) OPM_VOLUMES_DIR=$(OPM_VOLUMES_DIR) OPM_DATA_DIR=$(OPM_DATA_DIR) OPM_WEB_DIR=$(OPM_WEB_DIR) cargo run -p opm-server

dev-web:
	npm --prefix web run dev

build:
	npm --prefix web run build && cargo build --release

clean:
	cargo clean

# Targets owned by other areas; absent until they land.
-include docker/docker.mk
-include docs/docs.mk
-include fixtures/fixtures.mk
-include tools/tools.mk
-include e2e/e2e.mk
