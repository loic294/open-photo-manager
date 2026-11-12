# Fixture library targets; the root Makefile includes this file (see fixtures/README.md).
#   make fixtures          small catalogue in fixtures/out (OPM_VOLUMES_DIR defaults to fixtures/out/volumes)
#   make fixtures-large    FIXTURES_FILES tiny files in fixtures/out-large (default 200000)
#   make fixtures-verify   re-check fixtures/out against its manifest
#   make fixtures-clean    remove both output directories
FIXTURES_OUT ?= fixtures/out
FIXTURES_LARGE_OUT ?= fixtures/out-large
FIXTURES_FILES ?= 200000

.PHONY: fixtures fixtures-large fixtures-verify fixtures-clean

fixtures:
	fixtures/generate.sh --out $(FIXTURES_OUT)

fixtures-large:
	fixtures/generate.sh --scale large --files $(FIXTURES_FILES) --out $(FIXTURES_LARGE_OUT)

fixtures-verify:
	fixtures/verify.sh --out $(FIXTURES_OUT)

fixtures-clean:
	fixtures/generate.sh --clean-only --out $(FIXTURES_OUT)
	fixtures/generate.sh --clean-only --out $(FIXTURES_LARGE_OUT)
