DOCKER_COMPOSE ?= docker compose
DOCKER_COMPOSE_FILE ?= docker-compose-local.yml
LOCAL_DOCKER_APP_SERVICES ?= backend
LOCAL_DOCKER_ALL_SERVICES ?= postgres redis arangodb backend worker nginx
RUSTRAG_BENCHMARK_BASE_URL ?= http://127.0.0.1:19000/v1
RUSTRAG_BENCHMARK_SUITES ?= apps/api/benchmarks/grounded_query/api_baseline_suite.json apps/api/benchmarks/grounded_query/workflow_strict_suite.json apps/api/benchmarks/grounded_query/layout_noise_suite.json apps/api/benchmarks/grounded_query/graph_multihop_suite.json apps/api/benchmarks/grounded_query/multiformat_surface_suite.json
RUSTRAG_GOLDEN_SUITES ?= apps/api/benchmarks/grounded_query/golden_programming_suite.json apps/api/benchmarks/grounded_query/golden_infrastructure_suite.json apps/api/benchmarks/grounded_query/golden_protocols_suite.json apps/api/benchmarks/grounded_query/golden_code_suite.json apps/api/benchmarks/grounded_query/golden_multiformat_suite.json
RUSTRAG_GOLDEN_OUTPUT_DIR ?= tmp-golden-benchmarks
RUSTRAG_BENCHMARK_OUTPUT_DIR ?= tmp-grounded-benchmarks
RUSTRAG_BENCHMARK_CANONICALIZE_REUSED_LIBRARY ?= 1
RUSTRAG_BENCHMARK_LIBRARY_NAME ?= Grounded Benchmark Seed
BACKEND_CARGO_TARGET_DIR ?= $(CURDIR)/.cargo-target/api
FRONTEND_CARGO_TARGET_DIR ?= $(CURDIR)/.cargo-target/web

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
	frontend-check \
	check \
	check-strict \
	enterprise-validate \
	audit \
	benchmark-grounded \
	benchmark-grounded-all \
	benchmark-grounded-seed \
	benchmark-grounded-noisy-layout \
	benchmark-grounded-multihop \
	benchmark-golden \
	benchmark-golden-seed \
	docker-local-build \
	docker-local-rebuild \
	docker-local-redeploy \
	docker-local-refresh \
	docker-local-up \
	docker-local-down

backend-fmt:
	cargo fmt --all

backend-build:
	CARGO_TARGET_DIR="$(BACKEND_CARGO_TARGET_DIR)" cargo build --release -p rustrag-backend --bin rustrag-backend --bin rebuild_runtime_graph

backend-lint:
	CARGO_TARGET_DIR="$(BACKEND_CARGO_TARGET_DIR)" cargo clippy -p rustrag-backend --all-targets --all-features -- -D warnings

backend-doc:
	CARGO_TARGET_DIR="$(BACKEND_CARGO_TARGET_DIR)" cargo doc -p rustrag-backend --no-deps

backend-test:
	CARGO_TARGET_DIR="$(BACKEND_CARGO_TARGET_DIR)" cargo test -p rustrag-backend

backend-change-gate:
	cargo fmt --all --check
	CARGO_TARGET_DIR="$(BACKEND_CARGO_TARGET_DIR)" cargo check -q -p rustrag-backend
	CARGO_TARGET_DIR="$(BACKEND_CARGO_TARGET_DIR)" cargo test -q -p rustrag-backend

backend-audit:
	CARGO_TARGET_DIR="$(BACKEND_CARGO_TARGET_DIR)" cargo audit

frontend-install:
	cd apps/web && npm ci

frontend-lint:
	cd apps/web && npx eslint . --max-warnings 0

frontend-typecheck:
	cd apps/web && npx tsc --noEmit

frontend-build:
	cd apps/web && npx vite build

frontend-check: frontend-typecheck frontend-build

check: backend-change-gate frontend-check

check-strict: backend-change-gate backend-doc frontend-check

enterprise-validate:
	$(MAKE) backend-change-gate
	$(MAKE) frontend-check

audit: backend-audit

benchmark-grounded:
	@test -n "$(RUSTRAG_SESSION_COOKIE)" || (echo "RUSTRAG_SESSION_COOKIE is required" && exit 1)
	@test -n "$(RUSTRAG_BENCHMARK_WORKSPACE_ID)" || (echo "RUSTRAG_BENCHMARK_WORKSPACE_ID is required" && exit 1)
	@mkdir -p "$(RUSTRAG_BENCHMARK_OUTPUT_DIR)"
	@set -- \
	  --base-url "$(RUSTRAG_BENCHMARK_BASE_URL)" \
	  --workspace-id "$(RUSTRAG_BENCHMARK_WORKSPACE_ID)" \
	  --session-cookie "$(RUSTRAG_SESSION_COOKIE)" \
	  --strict \
	  --output-dir "$(RUSTRAG_BENCHMARK_OUTPUT_DIR)"; \
	for suite in $(RUSTRAG_BENCHMARK_SUITES); do \
	  set -- "$$@" --suite "$$suite"; \
	done; \
	if [ -n "$(RUSTRAG_BENCHMARK_LIBRARY_ID)" ]; then \
	  set -- "$$@" --library-id "$(RUSTRAG_BENCHMARK_LIBRARY_ID)" --skip-upload; \
	  if [ "$(RUSTRAG_BENCHMARK_CANONICALIZE_REUSED_LIBRARY)" = "1" ]; then \
	    set -- "$$@" --canonicalize-reused-library; \
	  fi; \
	fi; \
	python3 apps/api/benchmarks/grounded_query/run_live_benchmark.py "$$@"

benchmark-grounded-all:
	@$(MAKE) benchmark-grounded

benchmark-grounded-seed:
	@test -n "$(RUSTRAG_SESSION_COOKIE)" || (echo "RUSTRAG_SESSION_COOKIE is required" && exit 1)
	@test -n "$(RUSTRAG_BENCHMARK_WORKSPACE_ID)" || (echo "RUSTRAG_BENCHMARK_WORKSPACE_ID is required" && exit 1)
	@mkdir -p "$(RUSTRAG_BENCHMARK_OUTPUT_DIR)"
	@library_name="$(RUSTRAG_BENCHMARK_LIBRARY_NAME)"; \
	if [ "$$library_name" = "Grounded Benchmark Seed" ]; then \
	  library_name="Grounded Benchmark Seed $$(date +%Y%m%d-%H%M%S)"; \
	fi; \
	set -- \
	  --base-url "$(RUSTRAG_BENCHMARK_BASE_URL)" \
	  --workspace-id "$(RUSTRAG_BENCHMARK_WORKSPACE_ID)" \
	  --session-cookie "$(RUSTRAG_SESSION_COOKIE)" \
	  --library-name "$$library_name" \
	  --upload-only \
	  --output-dir "$(RUSTRAG_BENCHMARK_OUTPUT_DIR)"; \
	for suite in $(RUSTRAG_BENCHMARK_SUITES); do \
	  set -- "$$@" --suite "$$suite"; \
	done; \
	if [ -n "$(RUSTRAG_BENCHMARK_LIBRARY_ID)" ]; then \
	  set -- "$$@" --library-id "$(RUSTRAG_BENCHMARK_LIBRARY_ID)"; \
	fi; \
	python3 apps/api/benchmarks/grounded_query/run_live_benchmark.py "$$@"

benchmark-grounded-noisy-layout:
	@$(MAKE) benchmark-grounded RUSTRAG_BENCHMARK_SUITES="apps/api/benchmarks/grounded_query/layout_noise_suite.json"

benchmark-grounded-multihop:
	@$(MAKE) benchmark-grounded RUSTRAG_BENCHMARK_SUITES="apps/api/benchmarks/grounded_query/graph_multihop_suite.json"

benchmark-golden:
	@$(MAKE) benchmark-grounded RUSTRAG_BENCHMARK_SUITES="$(RUSTRAG_GOLDEN_SUITES)" RUSTRAG_BENCHMARK_OUTPUT_DIR="$(RUSTRAG_GOLDEN_OUTPUT_DIR)" RUSTRAG_BENCHMARK_LIBRARY_NAME="Golden Benchmark"

benchmark-golden-seed:
	@$(MAKE) benchmark-grounded-seed RUSTRAG_BENCHMARK_SUITES="$(RUSTRAG_GOLDEN_SUITES)" RUSTRAG_BENCHMARK_OUTPUT_DIR="$(RUSTRAG_GOLDEN_OUTPUT_DIR)" RUSTRAG_BENCHMARK_LIBRARY_NAME="Golden Benchmark Seed"

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
