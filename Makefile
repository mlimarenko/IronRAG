DOCKER_COMPOSE ?= docker compose
DOCKER_COMPOSE_FILE ?= docker-compose-local.yml
LOCAL_DOCKER_APP_SERVICES ?= backend frontend
LOCAL_DOCKER_ALL_SERVICES ?= postgres redis arangodb backend frontend
RUSTRAG_BENCHMARK_BASE_URL ?= http://127.0.0.1:19000/v1
RUSTRAG_BENCHMARK_SUITES ?= backend/benchmarks/grounded_query/api_baseline_suite.json backend/benchmarks/grounded_query/workflow_strict_suite.json backend/benchmarks/grounded_query/layout_noise_suite.json backend/benchmarks/grounded_query/graph_multihop_suite.json backend/benchmarks/grounded_query/multiformat_surface_suite.json
RUSTRAG_BENCHMARK_OUTPUT_DIR ?= tmp-grounded-benchmarks
RUSTRAG_BENCHMARK_CANONICALIZE_REUSED_LIBRARY ?= 1
RUSTRAG_BENCHMARK_LIBRARY_NAME ?= Grounded Benchmark Seed
BACKEND_CARGO_TARGET_DIR ?= $(CURDIR)/backend/.cargo-target

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
	benchmark-grounded \
	benchmark-grounded-all \
	benchmark-grounded-seed \
	benchmark-grounded-noisy-layout \
	benchmark-grounded-multihop \
	docker-local-build \
	docker-local-rebuild \
	docker-local-redeploy \
	docker-local-refresh \
	docker-local-up \
	docker-local-down

backend-fmt:
	cd backend && cargo fmt --all

backend-build:
	cd backend && CARGO_TARGET_DIR="$(BACKEND_CARGO_TARGET_DIR)" cargo build --release

backend-lint:
	cd backend && CARGO_TARGET_DIR="$(BACKEND_CARGO_TARGET_DIR)" cargo clippy --all-targets --all-features -- -D warnings

backend-doc:
	cd backend && CARGO_TARGET_DIR="$(BACKEND_CARGO_TARGET_DIR)" cargo doc --no-deps

backend-test:
	cd backend && CARGO_TARGET_DIR="$(BACKEND_CARGO_TARGET_DIR)" cargo test

backend-change-gate:
	cd backend && CARGO_TARGET_DIR="$(BACKEND_CARGO_TARGET_DIR)" $(MAKE) change-gate

backend-audit:
	cd backend && CARGO_TARGET_DIR="$(BACKEND_CARGO_TARGET_DIR)" cargo audit

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

benchmark-grounded:
	@test -n "$(RUSTRAG_SESSION_COOKIE)" || (echo "RUSTRAG_SESSION_COOKIE is required" && exit 1)
	@test -n "$(RUSTRAG_BENCHMARK_WORKSPACE_ID)" || (echo "RUSTRAG_BENCHMARK_WORKSPACE_ID is required" && exit 1)
	@mkdir -p "$(RUSTRAG_BENCHMARK_OUTPUT_DIR)"
	@args="--base-url $(RUSTRAG_BENCHMARK_BASE_URL) --workspace-id $(RUSTRAG_BENCHMARK_WORKSPACE_ID) --session-cookie $(RUSTRAG_SESSION_COOKIE) --strict --output-dir $(RUSTRAG_BENCHMARK_OUTPUT_DIR)"; \
	for suite in $(RUSTRAG_BENCHMARK_SUITES); do \
	  args="$$args --suite $$suite"; \
	done; \
	if [ -n "$(RUSTRAG_BENCHMARK_LIBRARY_ID)" ]; then \
	  args="$$args --library-id $(RUSTRAG_BENCHMARK_LIBRARY_ID) --skip-upload"; \
	  if [ "$(RUSTRAG_BENCHMARK_CANONICALIZE_REUSED_LIBRARY)" = "1" ]; then \
	    args="$$args --canonicalize-reused-library"; \
	  fi; \
	fi; \
	python3 backend/benchmarks/grounded_query/run_live_benchmark.py $$args

benchmark-grounded-all:
	@$(MAKE) benchmark-grounded

benchmark-grounded-seed:
	@test -n "$(RUSTRAG_SESSION_COOKIE)" || (echo "RUSTRAG_SESSION_COOKIE is required" && exit 1)
	@test -n "$(RUSTRAG_BENCHMARK_WORKSPACE_ID)" || (echo "RUSTRAG_BENCHMARK_WORKSPACE_ID is required" && exit 1)
	@mkdir -p "$(RUSTRAG_BENCHMARK_OUTPUT_DIR)"
	@args="--base-url $(RUSTRAG_BENCHMARK_BASE_URL) --workspace-id $(RUSTRAG_BENCHMARK_WORKSPACE_ID) --session-cookie $(RUSTRAG_SESSION_COOKIE) --library-name \"$(RUSTRAG_BENCHMARK_LIBRARY_NAME)\" --upload-only --output-dir $(RUSTRAG_BENCHMARK_OUTPUT_DIR)"; \
	for suite in $(RUSTRAG_BENCHMARK_SUITES); do \
	  args="$$args --suite $$suite"; \
	done; \
	if [ -n "$(RUSTRAG_BENCHMARK_LIBRARY_ID)" ]; then \
	  args="$$args --library-id $(RUSTRAG_BENCHMARK_LIBRARY_ID)"; \
	fi; \
	python3 backend/benchmarks/grounded_query/run_live_benchmark.py $$args

benchmark-grounded-noisy-layout:
	@$(MAKE) benchmark-grounded RUSTRAG_BENCHMARK_SUITES="backend/benchmarks/grounded_query/layout_noise_suite.json"

benchmark-grounded-multihop:
	@$(MAKE) benchmark-grounded RUSTRAG_BENCHMARK_SUITES="backend/benchmarks/grounded_query/graph_multihop_suite.json"

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
