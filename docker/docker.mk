# Docker targets for the production image (see docker/README.md). Included by the root Makefile.
# Compatible with GNU make 3.81, the version macOS ships.

OPM_IMAGE ?= open-photo-manager:dev
OPM_PORT ?= 8080
# Folder whose sub-folders become volumes inside the container; an empty scratch folder when fixtures are absent.
OPM_RUN_VOLUMES ?= $(if $(wildcard fixtures/out/volumes),fixtures/out/volumes,target/docker-run/volumes)
OPM_RUN_DATA ?= target/docker-run/data

.PHONY: docker-build docker-run docker-smoke
.PHONY: check-compose

check: check-compose

check-compose:
	PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s docker -p 'test_check_compose.py'
	python3 docker/check_compose.py
	docker compose -f docker/compose.yml config -q

# Builds the image for this machine's architecture and loads it into the local Docker engine.
docker-build:
	scripts/build-image.sh --tag $(OPM_IMAGE)

# Runs the image like the compose example does: your own uid, read-only root filesystem, no capabilities.
docker-run: docker-build
	mkdir -p "$(OPM_RUN_VOLUMES)" "$(OPM_RUN_DATA)"
	docker run --rm --name opm-run \
		--user "$$(id -u):$$(id -g)" \
		--read-only --tmpfs /tmp --cap-drop ALL --security-opt no-new-privileges:true \
		-p $(OPM_PORT):8080 \
		-v "$(abspath $(OPM_RUN_VOLUMES)):/volumes" \
		-v "$(abspath $(OPM_RUN_DATA)):/data" \
		$(OPM_IMAGE)

# Builds the image and checks it end to end (tools, health, arbitrary uid, read-only root); cleans up after itself.
docker-smoke:
	docker/smoke.sh
