# Targets of tools/: the mock Immich server and the checks of the repository tooling. The root
# Makefile includes this file (see tools/mock-immich/README.md).
#   make check-tools          the gate of tools/ and e2e/: every check-* target below
#   make mock-immich          run the mock Immich server (default scenario, port 12283)
#   make mock-immich-smoke    drive the real backend (cargo build -p opm-server first) against the mock
# Options of `make mock-immich`, all optional, from the environment or the make command line:
#   MOCK_IMMICH_MOUNTS    comma-separated PREFIX=DIR pairs: the real directory Immich sees at PREFIX,
#                         e.g. /mnt/photos=fixtures/out/volumes/photos,/mnt/archive=fixtures/out/volumes/archive
#                         (no commas or spaces in a path)
#   MOCK_IMMICH_PORT      listening port (default 12283; 0 picks a free one)
#   MOCK_IMMICH_SCENARIO  scenario file (default: scenarios/default.json)
MOCK_IMMICH_DIR := tools/mock-immich
MOCK_IMMICH_MOUNTS ?=
MOCK_IMMICH_PORT ?=
MOCK_IMMICH_SCENARIO ?=
comma := ,

.PHONY: check-tools check-mock-immich mock-immich mock-immich-smoke

check-tools: check-mock-immich

check-mock-immich:
	scripts/ensure-web-deps.sh
	cd $(MOCK_IMMICH_DIR) && ../../web/node_modules/.bin/biome check . && ../../web/node_modules/.bin/tsc --noEmit -p . && node --test "test/*.test.ts"

mock-immich:
	node $(MOCK_IMMICH_DIR)/server.ts $(if $(MOCK_IMMICH_PORT),--port $(MOCK_IMMICH_PORT)) $(if $(MOCK_IMMICH_SCENARIO),--scenario '$(MOCK_IMMICH_SCENARIO)') $(foreach mount,$(subst $(comma), ,$(MOCK_IMMICH_MOUNTS)),--mount '$(mount)')

mock-immich-smoke:
	node $(MOCK_IMMICH_DIR)/smoke.mjs
