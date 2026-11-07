# Docker targets for the production image (see docker/README.md). Included by the root Makefile.
# Compatible with GNU make 3.81, the version macOS ships.

APM_IMAGE ?= advanced-photo-manager:dev
APM_PORT ?= 8080
# Folder whose sub-folders become volumes inside the container; an empty scratch folder when fixtures are absent.
APM_RUN_VOLUMES ?= $(if $(wildcard fixtures/out/volumes),fixtures/out/volumes,target/docker-run/volumes)
APM_RUN_DATA ?= target/docker-run/data

.PHONY: docker-build docker-run docker-smoke

# Builds the image for this machine's architecture and loads it into the local Docker engine.
docker-build:
	scripts/build-image.sh --tag $(APM_IMAGE)

# Runs the image like the compose example does: your own uid, read-only root filesystem, no capabilities.
docker-run: docker-build
	mkdir -p "$(APM_RUN_VOLUMES)" "$(APM_RUN_DATA)"
	docker run --rm --name apm-run \
		--user "$$(id -u):$$(id -g)" \
		--read-only --tmpfs /tmp --cap-drop ALL --security-opt no-new-privileges:true \
		-p $(APM_PORT):8080 \
		-v "$(abspath $(APM_RUN_VOLUMES)):/volumes" \
		-v "$(abspath $(APM_RUN_DATA)):/data" \
		$(APM_IMAGE)

# Builds the image and checks it end to end (tools, health, arbitrary uid, read-only root); cleans up after itself.
docker-smoke:
	docker/smoke.sh
