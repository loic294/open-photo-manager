.PHONY: e2e-install check-e2e e2e e2e-smoke e2e-headed e2e-report e2e-build

e2e-install:
	npm --prefix e2e ci
	cd e2e && npx playwright install chromium

check-e2e:
	test -d e2e/node_modules || npm --prefix e2e ci
	npm --prefix e2e run check

check-tools: check-e2e

e2e-build:
	test -d web/node_modules || npm --prefix web ci
	npm --prefix web run build
	cargo build -p opm-server $(if $(filter release,$(E2E_PROFILE)),--release)

e2e: e2e-build check-e2e
	cd e2e && npx playwright test $(E2E_ARGS)

e2e-smoke: e2e-build check-e2e
	cd e2e && npx playwright test --grep @smoke $(E2E_ARGS)

e2e-headed: e2e-build check-e2e
	cd e2e && npx playwright test --headed $(E2E_ARGS)

e2e-report:
	cd e2e && npx playwright show-report
