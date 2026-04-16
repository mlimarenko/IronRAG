DOCKER_COMPOSE ?= docker compose
DOCKER_COMPOSE_FILE ?= docker-compose-local.yml
LOCAL_DOCKER_APP_SERVICES ?= backend
LOCAL_DOCKER_ALL_SERVICES ?= postgres redis arangodb backend worker nginx
IRONRAG_BENCHMARK_BASE_URL ?= http://127.0.0.1:19000/v1
IRONRAG_BENCHMARK_SUITES ?= apps/api/benchmarks/grounded_query/api_baseline_suite.json apps/api/benchmarks/grounded_query/workflow_strict_suite.json apps/api/benchmarks/grounded_query/layout_noise_suite.json apps/api/benchmarks/grounded_query/graph_multihop_suite.json apps/api/benchmarks/grounded_query/multiformat_surface_suite.json
IRONRAG_TECHNICAL_SUITES ?= apps/api/benchmarks/grounded_query/technical_contract_suite.json
IRONRAG_GOLDEN_SUITES ?= apps/api/benchmarks/grounded_query/golden_programming_suite.json apps/api/benchmarks/grounded_query/golden_infrastructure_suite.json apps/api/benchmarks/grounded_query/golden_protocols_suite.json apps/api/benchmarks/grounded_query/golden_code_suite.json apps/api/benchmarks/grounded_query/golden_multiformat_suite.json
IRONRAG_GOLDEN_OUTPUT_DIR ?= tmp-golden-benchmarks
IRONRAG_BENCHMARK_OUTPUT_DIR ?= tmp-grounded-benchmarks
IRONRAG_BENCHMARK_CANONICALIZE_REUSED_LIBRARY ?= 1
IRONRAG_BENCHMARK_LIBRARY_NAME ?= Grounded Benchmark Seed
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
	benchmark-grounded-technical \
	benchmark-grounded-technical-seed \
	benchmark-golden \
	benchmark-golden-seed \
	docker-local-build \
	docker-local-rebuild \
	docker-local-redeploy \
	docker-local-refresh \
	docker-local-up \
	docker-local-down \
	perf-probe \
	agent-perf-probe \
	agent-perf-suite

backend-fmt:
	cargo fmt --all

backend-build:
	CARGO_TARGET_DIR="$(BACKEND_CARGO_TARGET_DIR)" cargo build --release -p ironrag-backend --bin ironrag-backend --bin rebuild_runtime_graph

backend-lint:
	CARGO_TARGET_DIR="$(BACKEND_CARGO_TARGET_DIR)" cargo clippy -p ironrag-backend --all-targets --all-features -- -D warnings

backend-doc:
	CARGO_TARGET_DIR="$(BACKEND_CARGO_TARGET_DIR)" cargo doc -p ironrag-backend --no-deps

backend-test:
	CARGO_TARGET_DIR="$(BACKEND_CARGO_TARGET_DIR)" cargo test -p ironrag-backend

backend-change-gate:
	cargo fmt --all --check
	CARGO_TARGET_DIR="$(BACKEND_CARGO_TARGET_DIR)" cargo check -q -p ironrag-backend
	CARGO_TARGET_DIR="$(BACKEND_CARGO_TARGET_DIR)" cargo test -q -p ironrag-backend

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
	@test -n "$(IRONRAG_SESSION_COOKIE)" || (echo "IRONRAG_SESSION_COOKIE is required" && exit 1)
	@test -n "$(IRONRAG_BENCHMARK_WORKSPACE_ID)" || (echo "IRONRAG_BENCHMARK_WORKSPACE_ID is required" && exit 1)
	@mkdir -p "$(IRONRAG_BENCHMARK_OUTPUT_DIR)"
	@set -- \
	  --base-url "$(IRONRAG_BENCHMARK_BASE_URL)" \
	  --workspace-id "$(IRONRAG_BENCHMARK_WORKSPACE_ID)" \
	  --session-cookie "$(IRONRAG_SESSION_COOKIE)" \
	  --strict \
	  --output-dir "$(IRONRAG_BENCHMARK_OUTPUT_DIR)"; \
	for suite in $(IRONRAG_BENCHMARK_SUITES); do \
	  set -- "$$@" --suite "$$suite"; \
	done; \
	if [ -n "$(IRONRAG_BENCHMARK_LIBRARY_ID)" ]; then \
	  set -- "$$@" --library-id "$(IRONRAG_BENCHMARK_LIBRARY_ID)" --skip-upload; \
	  if [ "$(IRONRAG_BENCHMARK_CANONICALIZE_REUSED_LIBRARY)" = "1" ]; then \
	    set -- "$$@" --canonicalize-reused-library; \
	  fi; \
	fi; \
	python3 apps/api/benchmarks/grounded_query/run_live_benchmark.py "$$@"

benchmark-grounded-all:
	@$(MAKE) benchmark-grounded

benchmark-grounded-seed:
	@test -n "$(IRONRAG_SESSION_COOKIE)" || (echo "IRONRAG_SESSION_COOKIE is required" && exit 1)
	@test -n "$(IRONRAG_BENCHMARK_WORKSPACE_ID)" || (echo "IRONRAG_BENCHMARK_WORKSPACE_ID is required" && exit 1)
	@mkdir -p "$(IRONRAG_BENCHMARK_OUTPUT_DIR)"
	@library_name="$(IRONRAG_BENCHMARK_LIBRARY_NAME)"; \
	if [ "$$library_name" = "Grounded Benchmark Seed" ]; then \
	  library_name="Grounded Benchmark Seed $$(date +%Y%m%d-%H%M%S)"; \
	fi; \
	set -- \
	  --base-url "$(IRONRAG_BENCHMARK_BASE_URL)" \
	  --workspace-id "$(IRONRAG_BENCHMARK_WORKSPACE_ID)" \
	  --session-cookie "$(IRONRAG_SESSION_COOKIE)" \
	  --library-name "$$library_name" \
	  --upload-only \
	  --output-dir "$(IRONRAG_BENCHMARK_OUTPUT_DIR)"; \
	for suite in $(IRONRAG_BENCHMARK_SUITES); do \
	  set -- "$$@" --suite "$$suite"; \
	done; \
	if [ -n "$(IRONRAG_BENCHMARK_LIBRARY_ID)" ]; then \
	  set -- "$$@" --library-id "$(IRONRAG_BENCHMARK_LIBRARY_ID)"; \
	fi; \
	python3 apps/api/benchmarks/grounded_query/run_live_benchmark.py "$$@"

benchmark-grounded-noisy-layout:
	@$(MAKE) benchmark-grounded IRONRAG_BENCHMARK_SUITES="apps/api/benchmarks/grounded_query/layout_noise_suite.json"

benchmark-grounded-multihop:
	@$(MAKE) benchmark-grounded IRONRAG_BENCHMARK_SUITES="apps/api/benchmarks/grounded_query/graph_multihop_suite.json"

benchmark-grounded-technical:
	@$(MAKE) benchmark-grounded IRONRAG_BENCHMARK_SUITES="$(IRONRAG_TECHNICAL_SUITES)" IRONRAG_BENCHMARK_OUTPUT_DIR="tmp-technical-benchmarks" IRONRAG_BENCHMARK_LIBRARY_NAME="Technical Benchmark"

benchmark-grounded-technical-seed:
	@$(MAKE) benchmark-grounded-seed IRONRAG_BENCHMARK_SUITES="$(IRONRAG_TECHNICAL_SUITES)" IRONRAG_BENCHMARK_OUTPUT_DIR="tmp-technical-benchmarks" IRONRAG_BENCHMARK_LIBRARY_NAME="Technical Benchmark Seed"

benchmark-golden:
	@$(MAKE) benchmark-grounded IRONRAG_BENCHMARK_SUITES="$(IRONRAG_GOLDEN_SUITES)" IRONRAG_BENCHMARK_OUTPUT_DIR="$(IRONRAG_GOLDEN_OUTPUT_DIR)" IRONRAG_BENCHMARK_LIBRARY_NAME="Golden Benchmark"

benchmark-golden-seed:
	@$(MAKE) benchmark-grounded-seed IRONRAG_BENCHMARK_SUITES="$(IRONRAG_GOLDEN_SUITES)" IRONRAG_BENCHMARK_OUTPUT_DIR="$(IRONRAG_GOLDEN_OUTPUT_DIR)" IRONRAG_BENCHMARK_LIBRARY_NAME="Golden Benchmark Seed"

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

perf-probe:
	python3 scripts/ops/profile-ui-endpoints.py

agent-perf-probe:
	@test -n "$(IRONRAG_AGENT_PROBE_LIBRARY_ID)" || (echo "IRONRAG_AGENT_PROBE_LIBRARY_ID is required" && exit 1)
	@set -- \
	  --base-url "$(or $(IRONRAG_AGENT_PROBE_BASE_URL),http://127.0.0.1:19000)" \
	  --login "$(or $(IRONRAG_AGENT_PROBE_LOGIN),admin)" \
	  --password "$(or $(IRONRAG_AGENT_PROBE_PASSWORD),rustrag123)" \
	  --library-id "$(IRONRAG_AGENT_PROBE_LIBRARY_ID)"; \
	if [ -n "$(IRONRAG_AGENT_PROBE_WORKSPACE_ID)" ]; then \
	  set -- "$$@" --workspace-id "$(IRONRAG_AGENT_PROBE_WORKSPACE_ID)"; \
	fi; \
	if [ -n "$(IRONRAG_MCP_TOKEN)" ]; then \
	  set -- "$$@" --mcp-token "$(IRONRAG_MCP_TOKEN)"; \
	fi; \
	if [ -n "$(IRONRAG_AGENT_PROBE_ENTITY_QUERY)" ]; then \
	  set -- "$$@" --entity-query "$(IRONRAG_AGENT_PROBE_ENTITY_QUERY)"; \
	fi; \
	if [ -n "$(IRONRAG_AGENT_PROBE_DOCUMENT_QUERY)" ]; then \
	  set -- "$$@" --document-query "$(IRONRAG_AGENT_PROBE_DOCUMENT_QUERY)"; \
	fi; \
	if [ -n "$(IRONRAG_AGENT_PROBE_DOCUMENT_LIMIT)" ]; then \
	  set -- "$$@" --document-limit "$(IRONRAG_AGENT_PROBE_DOCUMENT_LIMIT)"; \
	fi; \
	if [ -n "$(IRONRAG_AGENT_PROBE_GRAPH_LIMIT)" ]; then \
	  set -- "$$@" --graph-limit "$(IRONRAG_AGENT_PROBE_GRAPH_LIMIT)"; \
	fi; \
	if [ -n "$(IRONRAG_AGENT_PROBE_READ_LENGTH)" ]; then \
	  set -- "$$@" --read-length "$(IRONRAG_AGENT_PROBE_READ_LENGTH)"; \
	fi; \
	if [ -n "$(IRONRAG_AGENT_PROBE_QUESTION)" ]; then \
	  set -- "$$@" --question "$(IRONRAG_AGENT_PROBE_QUESTION)"; \
	fi; \
	if [ -n "$(IRONRAG_AGENT_PROBE_SSE_RUNS)" ]; then \
	  set -- "$$@" --sse-runs "$(IRONRAG_AGENT_PROBE_SSE_RUNS)"; \
	fi; \
	if [ -n "$(IRONRAG_AGENT_PROBE_ENTITY_SEARCH_MIN_HITS)" ]; then \
	  set -- "$$@" --entity-search-min-hits "$(IRONRAG_AGENT_PROBE_ENTITY_SEARCH_MIN_HITS)"; \
	fi; \
	if [ -n "$(IRONRAG_AGENT_PROBE_GRAPH_MIN_ENTITIES)" ]; then \
	  set -- "$$@" --graph-min-entities "$(IRONRAG_AGENT_PROBE_GRAPH_MIN_ENTITIES)"; \
	fi; \
	if [ -n "$(IRONRAG_AGENT_PROBE_GRAPH_MIN_RELATIONS)" ]; then \
	  set -- "$$@" --graph-min-relations "$(IRONRAG_AGENT_PROBE_GRAPH_MIN_RELATIONS)"; \
	fi; \
	if [ -n "$(IRONRAG_AGENT_PROBE_GRAPH_MIN_DOCUMENTS)" ]; then \
	  set -- "$$@" --graph-min-documents "$(IRONRAG_AGENT_PROBE_GRAPH_MIN_DOCUMENTS)"; \
	fi; \
	if [ -n "$(IRONRAG_AGENT_PROBE_COMMUNITY_MIN_COUNT)" ]; then \
	  set -- "$$@" --community-min-count "$(IRONRAG_AGENT_PROBE_COMMUNITY_MIN_COUNT)"; \
	fi; \
	if [ -n "$(IRONRAG_AGENT_PROBE_SEARCH_MIN_HITS)" ]; then \
	  set -- "$$@" --search-min-hits "$(IRONRAG_AGENT_PROBE_SEARCH_MIN_HITS)"; \
	fi; \
	if [ -n "$(IRONRAG_AGENT_PROBE_SEARCH_MIN_READABLE_HITS)" ]; then \
	  set -- "$$@" --search-min-readable-hits "$(IRONRAG_AGENT_PROBE_SEARCH_MIN_READABLE_HITS)"; \
	fi; \
	if [ -n "$(IRONRAG_AGENT_PROBE_READ_MIN_CONTENT_CHARS)" ]; then \
	  set -- "$$@" --read-min-content-chars "$(IRONRAG_AGENT_PROBE_READ_MIN_CONTENT_CHARS)"; \
	fi; \
	if [ -n "$(IRONRAG_AGENT_PROBE_READ_MIN_REFERENCES)" ]; then \
	  set -- "$$@" --read-min-references "$(IRONRAG_AGENT_PROBE_READ_MIN_REFERENCES)"; \
	fi; \
	if [ -n "$(IRONRAG_AGENT_PROBE_ASSISTANT_MIN_REFERENCES)" ]; then \
	  set -- "$$@" --assistant-min-references "$(IRONRAG_AGENT_PROBE_ASSISTANT_MIN_REFERENCES)"; \
	fi; \
	if [ -n "$(IRONRAG_AGENT_PROBE_ASSISTANT_EXPECTED_VERIFICATION)" ]; then \
	  set -- "$$@" --assistant-expected-verification "$(IRONRAG_AGENT_PROBE_ASSISTANT_EXPECTED_VERIFICATION)"; \
	fi; \
	if [ -n "$(IRONRAG_AGENT_PROBE_ASSISTANT_REQUIRE_ALL)" ]; then \
	  set -- "$$@" --assistant-require-all "$(IRONRAG_AGENT_PROBE_ASSISTANT_REQUIRE_ALL)"; \
	fi; \
	if [ -n "$(IRONRAG_AGENT_PROBE_ASSISTANT_FORBID_ANY)" ]; then \
	  set -- "$$@" --assistant-forbid-any "$(IRONRAG_AGENT_PROBE_ASSISTANT_FORBID_ANY)"; \
	fi; \
	if [ -n "$(IRONRAG_AGENT_PROBE_ASSISTANT_MAX_TOOL_STARTS)" ]; then \
	  set -- "$$@" --assistant-max-tool-starts "$(IRONRAG_AGENT_PROBE_ASSISTANT_MAX_TOOL_STARTS)"; \
	fi; \
	if [ -n "$(IRONRAG_AGENT_PROBE_EXPECTED_SEARCH_TOP_LABEL)" ]; then \
	  set -- "$$@" --expected-search-top-label "$(IRONRAG_AGENT_PROBE_EXPECTED_SEARCH_TOP_LABEL)"; \
	fi; \
	if [ -n "$(IRONRAG_AGENT_PROBE_MAX_TOOL_LATENCY_MS)" ]; then \
	  set -- "$$@" --max-tool-latency-ms "$(IRONRAG_AGENT_PROBE_MAX_TOOL_LATENCY_MS)"; \
	fi; \
	if [ -n "$(IRONRAG_AGENT_PROBE_MAX_FIRST_DELTA_MS)" ]; then \
	  set -- "$$@" --max-first-delta-ms "$(IRONRAG_AGENT_PROBE_MAX_FIRST_DELTA_MS)"; \
	fi; \
	if [ -n "$(IRONRAG_AGENT_PROBE_MAX_COMPLETED_MS)" ]; then \
	  set -- "$$@" --max-completed-ms "$(IRONRAG_AGENT_PROBE_MAX_COMPLETED_MS)"; \
	fi; \
	python3 scripts/ops/profile-agent-surfaces.py "$$@"

agent-perf-suite:
	@test -n "$(IRONRAG_AGENT_PROBE_LIBRARY_ID)" || (echo "IRONRAG_AGENT_PROBE_LIBRARY_ID is required" && exit 1)
	@set -- \
	  --suite-path "$(or $(IRONRAG_AGENT_PROBE_SUITE_PATH),scripts/ops/agent-surface-suite.json)" \
	  --base-url "$(or $(IRONRAG_AGENT_PROBE_BASE_URL),http://127.0.0.1:19000)" \
	  --login "$(or $(IRONRAG_AGENT_PROBE_LOGIN),admin)" \
	  --password "$(or $(IRONRAG_AGENT_PROBE_PASSWORD),rustrag123)" \
	  --library-id "$(IRONRAG_AGENT_PROBE_LIBRARY_ID)"; \
	if [ -n "$(IRONRAG_AGENT_PROBE_WORKSPACE_ID)" ]; then \
	  set -- "$$@" --workspace-id "$(IRONRAG_AGENT_PROBE_WORKSPACE_ID)"; \
	fi; \
	if [ -n "$(IRONRAG_MCP_TOKEN)" ]; then \
	  set -- "$$@" --mcp-token "$(IRONRAG_MCP_TOKEN)"; \
	fi; \
	if [ -n "$(IRONRAG_AGENT_PROBE_REPORTS_DIR)" ]; then \
	  set -- "$$@" --reports-dir "$(IRONRAG_AGENT_PROBE_REPORTS_DIR)"; \
	fi; \
	if [ -n "$(IRONRAG_AGENT_PROBE_SUITE_OUTPUT_PATH)" ]; then \
	  set -- "$$@" --output-path "$(IRONRAG_AGENT_PROBE_SUITE_OUTPUT_PATH)"; \
	fi; \
	python3 scripts/ops/run-agent-surface-suite.py "$$@"
