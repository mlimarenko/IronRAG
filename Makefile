DOCKER_COMPOSE ?= docker compose
DOCKER_COMPOSE_FILE ?= docker-compose.yml
LOCAL_DOCKER_APP_SERVICES ?= backend frontend
LOCAL_DOCKER_ALL_SERVICES ?= postgres redis arangodb backend frontend

.PHONY: \
	backend-fmt \
	backend-build \
	backend-lint \
	backend-doc \
	backend-test \
	backend-change-gate \
	backend-audit \
	frontend-install \
	frontend-lint \
	frontend-format-check \
	frontend-typecheck \
	frontend-build \
	check \
	check-strict \
	enterprise-validate \
	audit \
	release-validation \
	docker-local-build \
	docker-local-rebuild \
	docker-local-redeploy \
	docker-local-refresh \
	docker-local-up \
	docker-local-down

backend-fmt:
	cd backend && cargo fmt --all

backend-build:
	cd backend && cargo build --release

backend-lint:
	cd backend && cargo clippy --all-targets --all-features -- -D warnings

backend-doc:
	cd backend && cargo doc --no-deps

backend-test:
	cd backend && cargo test

backend-change-gate:
	cd backend && $(MAKE) change-gate

backend-audit:
	cd backend && cargo audit

frontend-install:
	cd frontend && npm install

frontend-lint:
	cd frontend && npm run lint

frontend-format-check:
	cd frontend && npm run format:check

frontend-typecheck:
	cd frontend && npm run typecheck

frontend-build:
	cd frontend && npm run build

check: backend-change-gate frontend-lint frontend-format-check frontend-typecheck

check-strict: backend-change-gate backend-doc frontend-lint frontend-format-check frontend-typecheck

enterprise-validate:
	$(MAKE) backend-change-gate
	cd frontend && npm run enterprise:check

audit: backend-audit

release-validation:
	node scripts/release-validation/run.mjs --library-id $(LIBRARY_ID)

docker-local-build:
	$(DOCKER_COMPOSE) -f $(DOCKER_COMPOSE_FILE) build $(LOCAL_DOCKER_APP_SERVICES)

docker-local-rebuild:
	$(DOCKER_COMPOSE) -f $(DOCKER_COMPOSE_FILE) build --no-cache $(LOCAL_DOCKER_APP_SERVICES)

docker-local-redeploy:
	$(DOCKER_COMPOSE) -f $(DOCKER_COMPOSE_FILE) up -d --force-recreate $(LOCAL_DOCKER_APP_SERVICES)

docker-local-refresh: docker-local-build docker-local-redeploy

docker-local-up:
	$(DOCKER_COMPOSE) -f $(DOCKER_COMPOSE_FILE) up -d $(LOCAL_DOCKER_ALL_SERVICES)

docker-local-down:
	$(DOCKER_COMPOSE) -f $(DOCKER_COMPOSE_FILE) down
