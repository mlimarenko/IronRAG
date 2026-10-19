DOCKER_COMPOSE ?= docker compose
DOCKER_COMPOSE_FILE ?= docker-compose.yml
DOCKER ?= docker
# Local source-build image names so `compose build` tags `:local` instead of
# overwriting the published `pipingspace/*:latest` tags (see docker-compose.yml).
LOCAL_BACKEND_IMAGE ?= ironrag-backend:local
LOCAL_FRONTEND_IMAGE ?= ironrag-frontend:local
LOCAL_IMAGE_ENV ?= IRONRAG_BACKEND_IMAGE=$(LOCAL_BACKEND_IMAGE) IRONRAG_FRONTEND_IMAGE=$(LOCAL_FRONTEND_IMAGE)
# Build identity baked into locally built images and served by /v1/version.
# Resolved on the host, where git works the same in a linked worktree as in the
# primary checkout — unlike inside the image, where .dockerignore excludes .git
# and a worktree's .git is a pointer to a path outside the build context.
# `--match ''` keeps this a commit id rather than the nearest tag, `--dirty`
# marks an image built from uncommitted edits, and a tree that is not a
# repository yields the empty string, which the Dockerfiles reject outright
# instead of baking a version that never existed.
IRONRAG_VERSION ?= $(shell git -C $(CURDIR) describe --always --abbrev=40 --dirty --match '' 2>/dev/null)
export IRONRAG_VERSION
LOCAL_DOCKER_BUILD_SERVICES ?= backend frontend
LOCAL_DOCKER_RUNTIME_SERVICES ?= startup backend worker frontend
LOCAL_DOCKER_STEADY_SERVICES = $(filter-out startup,$(LOCAL_DOCKER_RUNTIME_SERVICES))
LOCAL_DOCKER_ALL_SERVICES ?= postgres redis startup backend worker frontend
export IRONRAG_BACKEND_MEMORY_LIMIT
export IRONRAG_WORKER_MEMORY_LIMIT
IRONRAG_BENCHMARK_BASE_URL ?= http://127.0.0.1:19000/v1
# The single self-contained benchmark registry: every suite lives under the
# golden_ prefix and every corpus file is referenced by at least one suite.
IRONRAG_GOLDEN_SUITES ?= apps/api/benchmarks/grounded_query/golden_programming_suite.json apps/api/benchmarks/grounded_query/golden_infrastructure_suite.json apps/api/benchmarks/grounded_query/golden_protocols_suite.json apps/api/benchmarks/grounded_query/golden_code_suite.json apps/api/benchmarks/grounded_query/golden_multiformat_suite.json apps/api/benchmarks/grounded_query/golden_hardening_suite.json apps/api/benchmarks/grounded_query/golden_api_baseline_suite.json apps/api/benchmarks/grounded_query/golden_workflow_strict_suite.json apps/api/benchmarks/grounded_query/golden_layout_noise_suite.json apps/api/benchmarks/grounded_query/golden_graph_multihop_suite.json apps/api/benchmarks/grounded_query/golden_upload_surface_suite.json apps/api/benchmarks/grounded_query/golden_technical_contract_suite.json
IRONRAG_BENCHMARK_SUITES ?= $(IRONRAG_GOLDEN_SUITES)
IRONRAG_GOLDEN_OUTPUT_DIR ?= tmp-golden-benchmarks
IRONRAG_BENCHMARK_OUTPUT_DIR ?= tmp-grounded-benchmarks
IRONRAG_BENCHMARK_NORMALIZE_REUSED_LIBRARY ?= 1
export IRONRAG_SESSION_COOKIE
IRONRAG_BENCHMARK_LIBRARY_NAME ?= Grounded Benchmark Seed
IRONRAG_BENCHMARK_BASELINE_DIR ?=
IRONRAG_BENCHMARK_CANDIDATE_DIR ?=
IRONRAG_BENCHMARK_MAX_LATENCY_REGRESSION_PERCENT ?= 10
BACKEND_CARGO_TARGET_DIR ?= $(CURDIR)/.cargo-target/api
GITLEAKS ?= gitleaks
GITLEAKS_LOG_OPTS ?=

.PHONY: \
	backend-fmt \
	backend-build \
	backend-lint \
	backend-doc \
	backend-test-integration \
	backend-test-lib-ignored \
	backend-test-fast \
	backend-test-provider-smoke \
	backend-bench-entity-type \
	backend-bench-graph-extraction \
	backend-bench-semantic-rerank \
	backend-change-gate \
	backend-audit \
	backend-deny \
	backend-unused-deps \
	backend-audit-strict \
	ops-selftest \
	ci-backend-gates \
	ci-backend-functional \
	ci-python-functional \
	ci-frontend-gates \
	ci-ops-gates \
	secret-check \
	static-max \
	migration-lint-self-test \
	migration-check \
	openapi-emit \
	openapi-check \
	openapi-public-check \
	frontend-sdk-check \
	contract-check \
	frontend-install \
	frontend-lint \
	frontend-format-check \
	frontend-typecheck \
	frontend-test \
	frontend-build \
	frontend-bundle-check \
	frontend-size-limit \
	frontend-coverage \
	frontend-i18n-audit \
	frontend-color-gate \
	frontend-deadcode \
	frontend-swagger-check \
	frontend-mocks-regen \
	frontend-check \
	compose-check \
	helm-chart-check \
	mem-budget-check \
	install-script-check \
	mcp-consumer-contract-check \
	pre-commit-install \
	check \
	check-strict \
	enterprise-validate \
	audit \
	benchmark-grounded \
	benchmark-grounded-seed \
	benchmark-grounded-noisy-layout \
	benchmark-grounded-multihop \
	benchmark-grounded-technical \
	benchmark-grounded-technical-seed \
	benchmark-golden \
	benchmark-golden-seed \
	benchmark-contract-test \
	benchmark-regression \
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

backend-fmt-check:
	cargo fmt --all --check

# Library tests need no Postgres or Redis. Database suites run separately in
# the local integration gate.
backend-test-lib:
	CARGO_TARGET_DIR="$(BACKEND_CARGO_TARGET_DIR)" cargo test --lib --all-features -p ironrag-backend

# The lib's database cases are `#[ignore]`d like the integration crate's, but
# they live in the unit-test binary because they exercise private items. A
# filter that matches nothing still exits 0, so each module's count is pinned
# here too, exactly as BACKEND_INTEGRATION_SUITES does for the `it` binary.
# Runs in the integration job, which carries the pgvector Postgres and Redis
# services these tests need; `--test-threads=1` because Redis is shared.
BACKEND_LIB_IGNORED_SUITES ?= \
  infra::postgres::pg_search_store::vector_support_tests:1 \
  infra::postgres::pg_search_store::vector_inventory::tests:5 \
  infra::repositories::webhook_outbox_repository::tests:4 \
  interfaces::http::mcp::tests:1 \
  services::graph::extract::persistence_retry::tests::native:1 \
  services::graph::gc::tests:2 \
  services::query::execution::semantic_rerank::binding_tests:2 \
  services::query::result_cache::tests:2 \
  services::query::search::tests:1

backend-test-lib-ignored:
	@log="$$(mktemp)"; \
	if DATABASE_URL="$$IRONRAG_DATABASE_URL" CARGO_TARGET_DIR="$(BACKEND_CARGO_TARGET_DIR)" cargo test --locked --color never \
	  -p ironrag-backend --lib --all-features -- --ignored --test-threads=1 \
	  --skip services::graph::extract::entity_type::bench:: \
	  --skip services::graph::extract::extraction_bench_tests:: \
	  --skip services::query::execution::semantic_rerank::rerank_bench_tests:: \
	  >"$$log" 2>&1; then \
	  cargo_status=0; \
	else \
	  cargo_status=$$?; \
	fi; \
	cat "$$log"; \
	if [ "$$cargo_status" -ne 0 ]; then rm -f "$$log"; exit "$$cargo_status"; fi; \
	python3 apps/api/scripts/check_integration_suite_counts.py \
	  --suites "$(BACKEND_LIB_IGNORED_SUITES)" "$$log"; \
	status=$$?; \
	rm -f "$$log"; \
	exit $$status

backend-build:
	CARGO_TARGET_DIR="$(BACKEND_CARGO_TARGET_DIR)" cargo build --release -p ironrag-backend --bins

backend-lint:
	CARGO_TARGET_DIR="$(BACKEND_CARGO_TARGET_DIR)" cargo clippy --workspace --all-targets --all-features -- -D warnings -D clippy::cognitive_complexity -A clippy::expect_used -A clippy::unwrap_used -A clippy::panic

backend-doc:
	RUSTDOCFLAGS="-D warnings" CARGO_TARGET_DIR="$(BACKEND_CARGO_TARGET_DIR)" cargo doc --workspace --no-deps

# Suites that need Postgres and Redis, each written as `<suite>:<test count>`.
# Kept as one explicit list so the gate and the coverage job cannot drift into
# testing different things, and so a suite that stops selecting the tests it
# used to select fails loudly instead of passing empty.
# How many tests the merged integration crate runs without a database. Every
# database case is `#[ignore]`d and runs in the integration job instead, so a
# plain run is exactly the set the compile job can execute in seconds. The
# number is here because a filter that matches nothing still exits 0 printing
# `test result: ok. 0 passed` -- the same trap the per-suite counts below exist
# to close.
# Derive it from cargo, never by hand:
#   cargo test -p ironrag-backend --all-features --test it 2>&1 | tail -1
BACKEND_FAST_TEST_COUNT ?= 43

BACKEND_INTEGRATION_SUITES ?= governance_auth:13 greenfield_bootstrap:36 \
  ai_catalog_repository:3 ai_catalog_service:6 ai_config_generation_fencing:4 atomic_admission:33 canonical_read_visibility:4 graph_visibility_race:1 mcp_public_writers:3 \
  billing_rollups:16 bootstrap_stack:3 cancellation_class:8 catalog_repository:16 credential_encryption:1 \
  iam_user_repository:2 incremental_graph_reconcile:2 knowledge_lifecycle:21 knowledge_search:24 login_rate_limit:3 \
  mcp_knowledge:3 \
  mcp_memory_contracts:6 mcp_memory_permissions:5 mcp_memory_reads:8 mcp_memory_search:11 \
  query_grounding:23 content_lifecycle:19 ingest_repository:18 ingest_attempts:6 \
  ingest_cancellation:2 ingest_preparation_fencing:3 ingest_unit_fencing:1 ingest_publication:16 \
  webhook_delivery:15 webhook_outbound:15 runtime_graph_extraction_repository:8 \
  runtime_graph_node_hints:1 runtime_orphan_reconcile:18 \
  audit_events:17 content_repository:21 content_document_graph_coverage:7 \
  structured_block_repository:1 content_revision_lineage:1 extract_graph:1 \
  extract_repository:1 graph_projection_repository:2 graph_atomicity_fencing:9 \
  graph_rebuild_guard_lifecycle:1 graph_reextract_repository:14 graph_snapshot_fencing:4 \
  graph_summary_executor_fencing:1 knowledge_revision_states:1 ops_state:6 \
  provider_batch_resolution:1 web_ingest_repository:8 web_ingest_recursive_crawl:5 \
  web_ingest_single_page:6 web_ingest_visibility:2 document_external_key_reclaim:3 \
  revision_prefix_fetch:1 snapshot_transfer:29 oidc_sso:9 \
  terminal_graph_state_repair:6 \
  stranded_fan_out_repair:9 \
  workspace_restore_atomicity:3

# `--features test-support` is required, not optional: six suites are entirely
# behind `#![cfg(feature = "test-support")]` and three more hide individual
# tests there. Without the flag those binaries contain no tests at all.
#
# And a test binary with no matching tests still exits 0, printing
# `test result: ok. 0 passed`. That is how 46 tests could have been "added to
# CI" and still never run, so the exit code cannot tell "everything passed"
# apart from "nothing ran". Every entry therefore states how many tests the
# suite must report, and any deviation -- zero, fewer, or more -- fails.
#
# Derive a count from cargo, never by hand:
#   cargo test --color never -p ironrag-backend --features test-support \
#     --test it -- --ignored --list <suite>::
# then count the lines ending in `: test`. `--color never` also keeps ANSI
# escapes out of the result line this recipe parses.
#
# `--test-threads=1` is a correctness requirement, not a courtesy: Postgres is
# isolated per test through temporary databases, but Redis is shared and not
# namespaced, so two tests in flight would see each other's keys.
#
# One run, then the counts are checked against what it reported. The suites used
# to be forty-eight separate cargo invocations, one per binary; they are modules
# of one binary now, and forty-eight filtered runs of it would pay the startup
# forty-eight times to learn the same thing.
#
# The one skip is the provider smoke test: it needs a live OpenAI key and fails
# without one. The per-suite loop never reached it because it was not in the
# list; an unfiltered run does, so the exclusion has to be stated.
# Run all non-ignored workspace targets once. The `it` count is checked inside
# its own Cargo binary section, so unrelated unit/binary totals cannot mask it.
# Merge Cargo's streams before the guard captures them: Cargo writes the binary
# headings to stderr and libtest writes the matching results to stdout.
backend-test-fast:
	@log="$$(mktemp)"; \
	if CARGO_TARGET_DIR="$(BACKEND_CARGO_TARGET_DIR)" sh -c 'exec "$$@" 2>&1' cargo-test \
	  cargo test --locked --color never --workspace --all-features --lib --bins --tests >"$$log" 2>&1; then \
	  cargo_status=0; \
	else \
	  cargo_status=$$?; \
	fi; \
	cat "$$log"; \
	if [ "$$cargo_status" -ne 0 ]; then rm -f "$$log"; exit "$$cargo_status"; fi; \
	python3 apps/api/scripts/check_integration_suite_counts.py \
	  --binary it --total "$(BACKEND_FAST_TEST_COUNT)" "$$log"; \
	status=$$?; \
	rm -f "$$log"; \
	exit $$status

backend-test-integration:
	@log="$$(mktemp)"; \
	if CARGO_TARGET_DIR="$(BACKEND_CARGO_TARGET_DIR)" cargo test --locked --color never -p ironrag-backend \
	  --all-features --test it -- --ignored --test-threads=1 \
	  --skip query_compiler_openai_smoke:: \
	  >"$$log" 2>&1; then \
	  cargo_status=0; \
	else \
	  cargo_status=$$?; \
	fi; \
	cat "$$log"; \
	if [ "$$cargo_status" -ne 0 ]; then \
	  rm -f "$$log"; \
	  echo "FAIL: cargo test exited $$cargo_status" >&2; \
	  exit "$$cargo_status"; \
	fi; \
	python3 apps/api/scripts/check_integration_suite_counts.py \
	  --suites "$(BACKEND_INTEGRATION_SUITES)" "$$log"; \
	status=$$?; \
	rm -f "$$log"; \
	exit $$status

# Paid, networked provider verification is intentionally separate from the
# deterministic integration lane. Both this wrapper and the Rust fixture reject
# a missing or unusable OpenAI credential instead of reporting a misleading
# green run that made zero calls.
backend-test-provider-smoke:
	@if [ -z "$${IRONRAG_AI_PROVIDER_API_KEYS_JSON_B64:-}" ]; then \
	  echo "FAIL provider smoke: IRONRAG_AI_PROVIDER_API_KEYS_JSON_B64 is required" >&2; \
	  exit 1; \
	fi
	CARGO_TARGET_DIR="$(BACKEND_CARGO_TARGET_DIR)" cargo test -p ironrag-backend \
	  --features test-support --test it -- --ignored --test-threads=1 query_compiler_openai_smoke::

# Paid A/B benchmark of the entity-typing stage over the frozen corpus in
# apps/api/benchmarks/graph_entity_type (IRG-184). Arms and credentials are read
# from ENTITY_TYPE_BENCH_* (see entity_type_bench_tests.rs); the run fails when no
# arm is configured. ENTITY_TYPE_BENCH_REPORT names the JSON report file. The
# report also carries an `llm_only` section: extract_graph over a synthetic
# label+summary fragment per batch, without the typing stage.
backend-bench-entity-type:
	CARGO_TARGET_DIR="$(BACKEND_CARGO_TARGET_DIR)" cargo test --color never \
	  -p ironrag-backend --lib --all-features -- --ignored --exact \
	  services::graph::extract::entity_type::bench::entity_type_classifier_benchmark

# Paid benchmark of graph extraction across chat models over the frozen public
# corpus in apps/api/benchmarks/graph_extraction (IRG-207). Arms and the report
# path are read from GRAPH_EXTRACTION_BENCH_* (see extraction_bench_tests.rs).
backend-bench-graph-extraction:
	CARGO_TARGET_DIR="$(BACKEND_CARGO_TARGET_DIR)" cargo test --color never \
	  -p ironrag-backend --lib --all-features -- --ignored --exact \
	  services::graph::extract::extraction_bench_tests::graph_extraction_model_benchmark

# Paid A/B benchmark of the optional semantic-rerank stage over the frozen
# labelled corpus in apps/api/benchmarks/semantic_rerank (IRG-186). Every arm
# runs the product dispatch `prepare_semantic_rerank_request` ->
# `call_rerank_provider`; the `off` arm keeps the corpus order. Credentials and
# arms come from SEMANTIC_RERANK_BENCH_* (see rerank_bench_tests.rs); the run
# fails when no arm is configured. SEMANTIC_RERANK_BENCH_REPORT names the JSON
# report file.
backend-bench-semantic-rerank:
	CARGO_TARGET_DIR="$(BACKEND_CARGO_TARGET_DIR)" cargo test --color never \
	  -p ironrag-backend --lib --all-features -- --ignored --exact \
	  services::query::execution::semantic_rerank::rerank_bench_tests::semantic_rerank_model_benchmark

backend-change-gate:
	cargo fmt --all --check
	CARGO_TARGET_DIR="$(BACKEND_CARGO_TARGET_DIR)" cargo check -q --workspace --all-targets --all-features
	CARGO_TARGET_DIR="$(BACKEND_CARGO_TARGET_DIR)" cargo test -q --workspace --all-targets --all-features

backend-audit:
	CARGO_TARGET_DIR="$(BACKEND_CARGO_TARGET_DIR)" cargo audit

backend-deny:
	cargo deny check

backend-unused-deps:
	cargo machete --with-metadata

backend-audit-strict: backend-audit backend-deny backend-unused-deps

ops-selftest:
	python3 -m unittest discover -s scripts/ops -p 'test_*.py'

# CI supplies an immutable commit range. Local runs inspect the current Git
# diff. The complete tracked/non-ignored worktree is always scanned as well, so
# historical fixtures cannot hide outside the current diff. No finding baseline
# or inline gitleaks:allow directive is accepted.
secret-check:
	python3 scripts/ops/scan_worktree_secrets.py --gitleaks "$(GITLEAKS)"
	@if [ -n "$(GITLEAKS_LOG_OPTS)" ]; then \
		"$(GITLEAKS)" git --redact --no-banner --no-color --ignore-gitleaks-allow --log-opts="$(GITLEAKS_LOG_OPTS)" .; \
	else \
		"$(GITLEAKS)" git --pre-commit --redact --no-banner --no-color --ignore-gitleaks-allow .; \
	fi

# Keep the authoritative migration-set policy and its isolated regression fixture executable.
migration-lint-self-test:
	bash scripts/ops/lint_migrations.sh --self-test

migration-check: migration-lint-self-test
	bash scripts/ops/lint_migrations.sh

# Regenerate the authoritative OpenAPI document from the utoipa annotations on
# Rust handlers and overwrite the committed copy. Run after every public API
# change so the spec served at /v1/openapi/ironrag.openapi.yaml stays in sync.
OPENAPI_CARGO_ARGS ?=
OPENAPI_CARGO = CARGO_TARGET_DIR="$(BACKEND_CARGO_TARGET_DIR)" cargo run $(OPENAPI_CARGO_ARGS) -q --bin ironrag-emit-openapi

openapi-emit:
	@tmp=$$(mktemp apps/api/contracts/.openapi.gen.yaml.XXXXXX) && \
	  trap 'rm -f "$$tmp"' EXIT INT TERM && \
	  $(OPENAPI_CARGO) > "$$tmp" && \
	  test -s "$$tmp" && \
	  mv "$$tmp" apps/api/contracts/openapi.gen.yaml && \
	  install -m 0644 apps/api/contracts/openapi.gen.yaml apps/web/public/openapi.gen.yaml

# Local drift check: regenerate the spec into a tmp file and diff it against the
# committed copy. Fails if a contributor forgot to refresh the contract.
openapi-check:
	@tmp=$$(mktemp) && \
	  $(OPENAPI_CARGO) > "$$tmp" && \
	  diff -u apps/api/contracts/openapi.gen.yaml "$$tmp" || { \
	    rm -f "$$tmp"; \
	    echo "OpenAPI drift detected. Run 'make openapi-emit' and commit the result."; \
	    exit 1; \
	  }; \
	rm -f "$$tmp"

# The frontend serves a copy of the authoritative contract. Keep it byte-identical
# so users never see an API description different from the backend source.
openapi-public-check:
	@cmp -s apps/api/contracts/openapi.gen.yaml apps/web/public/openapi.gen.yaml || { \
		echo "Frontend OpenAPI copy is stale. Sync apps/web/public/openapi.gen.yaml."; \
		exit 1; \
	}

# Regenerate the TypeScript client into a disposable sibling directory and
# compare it without touching tracked files.
frontend-sdk-check:
	@cd apps/web && \
	  out=$$(mktemp -d src/shared/api/.openapi-check.XXXXXX) && \
	  trap 'rm -rf "$$out"' EXIT INT TERM && \
	  ./node_modules/.bin/openapi-ts --output "$$out" --silent --no-log-file && \
	  node scripts/normalize-generated-sdk.mjs "$$out" && \
	  diff -ru src/shared/api/generated "$$out"

contract-check: openapi-check openapi-public-check frontend-sdk-check

frontend-install:
	cd apps/web && npm ci

frontend-lint:
	cd apps/web && ./node_modules/.bin/eslint .

frontend-format-check:
	cd apps/web && npm run format:check

frontend-typecheck:
	cd apps/web && npm run typecheck

frontend-test:
	cd apps/web && ./node_modules/.bin/vitest run

frontend-coverage:
	cd apps/web && npm run test:coverage && test -s coverage/lcov.info

# Browser scenarios. A Playwright run that collects nothing still exits 0, so
# the run is compared with apps/web/tests/e2e/spec-manifest.json -- the same
# reason BACKEND_INTEGRATION_SUITES exists one level down. The gate is a node
# script, not a Make step, because CI runs this in Playwright's own image,
# which carries the browser but neither make nor python3.
frontend-e2e:
	cd apps/web && npm run test:e2e

frontend-build:
	cd apps/web && ./node_modules/.bin/vite build

# Asserts the first-paint chunks stay under their hand-set ceilings. Sprint 7
# lazy-route work brought main from ~810 KB gzip to ~85 KB gzip; this gate
# stops a future commit from re-eagerizing a heavy page (Sigma, Tiptap,
# Swagger UI) without anyone noticing. Runs after build because it reads
# `dist/assets/*.js`.
frontend-bundle-check:
	cd apps/web && node scripts/check-bundle-budget.mjs

frontend-size-limit:
	cd apps/web && npm run size-limit

frontend-i18n-audit:
	cd apps/web && node scripts/i18n-audit.mjs

# Redesign guardrail: no raw Tailwind
# palette color literals in feature/app code. Status meaning routes through the
# semantic .status-*/--status-* tokens; the accent routes through --primary. A
# raw `bg-amber-50`/`text-emerald-600`/... is how pages drifted "slightly
# different" — a warning was amber on one page and status-warning on another.
frontend-color-gate:
	@if grep -rEn '(text|bg|border|ring|from|to|via)-(amber|yellow|orange|emerald|green|teal|lime|red|rose|blue|sky|cyan|violet|purple|indigo|pink|slate|gray|zinc|neutral)-[0-9]' apps/web/src/features apps/web/src/app --include='*.tsx' --include='*.ts'; then \
		echo 'ERROR: raw Tailwind palette color literal above — use .status-*/--status-* or --primary tokens'; \
		exit 1; \
	fi

# knip is the single dead-code authority here: it reports unused files, exports
# and dependencies, and it fails the build. ts-prune used to run alongside it
# and was removed on 2026-08-16 — it exited 0 whatever it found, so it could
# never fail, and it emitted 1082 lines dominated by barrel re-exports it
# cannot follow. A check that cannot fail reads as coverage without being it.
frontend-deadcode:
	cd apps/web && ./node_modules/.bin/knip --reporter compact

frontend-swagger-check:
	cd apps/web && npm run swagger:check

# Sprint 5: regenerate apps/web/src/api/mocks/handlers.ts from the authoritative
# OpenAPI doc. Run after `make openapi-emit` whenever endpoints
# change. The generator emits one default `http.<method>` per operation
# returning `HttpResponse.json({})` so MSW always has a seed handler.
frontend-mocks-regen:
	cd apps/web && node scripts/gen-msw-handlers.mjs

# Sprint 8: authoritative frontend gate. Hard-failing rules (`error` level in
# eslint.config.js — Sprint 2d's no-restricted-syntax + the typecheck and
# bundle-budget gates) keep the gate green; the lint pass tolerates the
# residual fast-refresh / strict-react-hooks warnings tracked separately.
frontend-check: frontend-lint frontend-typecheck frontend-color-gate frontend-i18n-audit frontend-test frontend-swagger-check frontend-build frontend-bundle-check frontend-size-limit

# ---------------------------------------------------------------------------
# Connectors (Python). One distribution, one lock file, one test run: a change
# to the shared framework is verified against every source that uses it, in the
# same pipeline, before it can merge.
# ---------------------------------------------------------------------------
CONNECTORS_DIR := connectors

connectors-sync:
	uv sync --directory $(CONNECTORS_DIR) --all-extras

connectors-test:
	uv run --directory $(CONNECTORS_DIR) --all-extras pytest -q

connectors-lint:
	# --no-cache: an editable sibling used to change import classification, and a
	# stale Ruff cache hid the resulting CI error.
	uv run --directory $(CONNECTORS_DIR) --all-extras ruff check --no-cache .

connectors-format-check:
	uv run --directory $(CONNECTORS_DIR) --all-extras ruff format --no-cache --check .

connectors-typecheck:
	uv run --directory $(CONNECTORS_DIR) --all-extras mypy src

connectors-lock-check:
	# Proves uv.lock matches pyproject.toml: a lock that drifts silently installs
	# something other than what the image will.
	uv lock --directory $(CONNECTORS_DIR) --check

connectors-gates: connectors-lint connectors-format-check connectors-typecheck connectors-lock-check connectors-test

# Full local Rust checks remain available alongside the functional CI path.
ci-backend-gates: backend-lint backend-test-fast migration-check openapi-check openapi-public-check

# Fast CI path: run each functional suite once without LLVM instrumentation.
ci-backend-functional: backend-test-fast backend-test-lib-ignored backend-test-integration migration-check openapi-check openapi-public-check

ci-python-functional: connectors-sync connectors-test ops-selftest

ci-frontend-gates: frontend-lint frontend-typecheck frontend-i18n-audit frontend-coverage frontend-color-gate frontend-deadcode frontend-swagger-check frontend-sdk-check

ci-ops-gates:
	$(MAKE) backend-unused-deps compose-check helm-chart-check mem-budget-check install-script-check mcp-consumer-contract-check benchmark-contract-test

static-max: ops-selftest backend-unused-deps frontend-format-check frontend-deadcode connectors-gates

# Parse the public example instead of the secret-bearing local `.env`.
compose-check:
	$(DOCKER_COMPOSE) --env-file .env.example -f $(DOCKER_COMPOSE_FILE) config --quiet
	@set -e; tmp="$$(mktemp -d)"; trap 'rm -rf "$$tmp"' EXIT INT TERM; \
	for dir in connectors/deploy/*/; do \
	  name="$$(basename "$$dir")"; \
	  cp -r "$$dir" "$$tmp/$$name"; \
	  for example in "$$tmp/$$name"/*.example; do \
	    [ -e "$$example" ] || continue; \
	    cp "$$example" "$${example%.example}"; \
	  done; \
	  mv "$$tmp/$$name/.env.example" "$$tmp/$$name/.env" 2>/dev/null || true; \
	  for file in "$$tmp/$$name"/docker-compose*.yml; do \
	    [ -e "$$file" ] || continue; \
	    $(DOCKER_COMPOSE) --project-directory "$$tmp/$$name" --env-file "$$tmp/$$name/.env" \
	      -f "$$file" config --quiet \
	      || { echo "FAIL compose-check: $$dir$$(basename $$file) does not parse from its own .env.example" >&2; exit 1; }; \
	  done; \
	done; \
	echo "compose-check: root stack and $$(ls -d connectors/deploy/*/ | wc -l) connector templates parse"

helm-chart-check:
	scripts/minikube/render-all.sh

# Guard the docker-compose memory budget: fail if the steady-state sum of
# `memory` LIMITS exceeds the swapless 16 GiB host ceiling (see the script
# and the "Memory containment" header in docker-compose.yml). Keeps a future
# edit from silently re-oversubscribing the host.
mem-budget-check:
	scripts/ops/check-mem-budget.sh

# Validate the install wizard: syntax, the resource-sizing table, the
# atomic/secret-safe .env merge, and (where a pty is available) the interactive
# prompts. No Docker or network required.
install-script-check:
	bash -n install.sh
	sh -n apps/api/docker/runtime-entrypoint.sh
	bash tests/install_wizard.test.sh
	bash tests/runtime_entrypoint.test.sh

mcp-consumer-contract-check:
	bash tests/mcp_consumer_contract_test.sh

check: static-max migration-check backend-change-gate contract-check frontend-check compose-check helm-chart-check mem-budget-check install-script-check mcp-consumer-contract-check benchmark-contract-test

check-strict: static-max migration-check backend-change-gate backend-lint backend-doc contract-check frontend-check compose-check helm-chart-check mem-budget-check install-script-check mcp-consumer-contract-check benchmark-contract-test

pre-commit-install:
	pre-commit install --install-hooks

enterprise-validate:
	$(MAKE) backend-change-gate
	$(MAKE) frontend-check

audit: backend-audit-strict

benchmark-grounded:
	@test -n "$$IRONRAG_SESSION_COOKIE" || (echo "IRONRAG_SESSION_COOKIE is required" && exit 1)
	@test -n "$(IRONRAG_BENCHMARK_WORKSPACE_ID)" || (echo "IRONRAG_BENCHMARK_WORKSPACE_ID is required" && exit 1)
	@mkdir -p "$(IRONRAG_BENCHMARK_OUTPUT_DIR)"
	@set -- \
	  --base-url "$(IRONRAG_BENCHMARK_BASE_URL)" \
	  --workspace-id "$(IRONRAG_BENCHMARK_WORKSPACE_ID)" \
	  --strict \
	  --output-dir "$(IRONRAG_BENCHMARK_OUTPUT_DIR)"; \
	for suite in $(IRONRAG_BENCHMARK_SUITES); do \
	  set -- "$$@" --suite "$$suite"; \
	done; \
	if [ -n "$(IRONRAG_BENCHMARK_LIBRARY_ID)" ]; then \
	  set -- "$$@" --library-id "$(IRONRAG_BENCHMARK_LIBRARY_ID)" --skip-upload; \
	  if [ "$(IRONRAG_BENCHMARK_NORMALIZE_REUSED_LIBRARY)" = "1" ]; then \
	    set -- "$$@" --canoni""calize-reused-library; \
	  fi; \
	fi; \
	python3 apps/api/benchmarks/grounded_query/run_live_benchmark.py "$$@"

benchmark-grounded-seed:
	@test -n "$$IRONRAG_SESSION_COOKIE" || (echo "IRONRAG_SESSION_COOKIE is required" && exit 1)
	@test -n "$(IRONRAG_BENCHMARK_WORKSPACE_ID)" || (echo "IRONRAG_BENCHMARK_WORKSPACE_ID is required" && exit 1)
	@mkdir -p "$(IRONRAG_BENCHMARK_OUTPUT_DIR)"
	@library_name="$(IRONRAG_BENCHMARK_LIBRARY_NAME)"; \
	if [ "$$library_name" = "Grounded Benchmark Seed" ]; then \
	  library_name="Grounded Benchmark Seed $$(date +%Y%m%d-%H%M%S)"; \
	fi; \
	set -- \
	  --base-url "$(IRONRAG_BENCHMARK_BASE_URL)" \
	  --workspace-id "$(IRONRAG_BENCHMARK_WORKSPACE_ID)" \
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
	@$(MAKE) benchmark-grounded IRONRAG_BENCHMARK_SUITES="apps/api/benchmarks/grounded_query/golden_layout_noise_suite.json"

benchmark-grounded-multihop:
	@$(MAKE) benchmark-grounded IRONRAG_BENCHMARK_SUITES="apps/api/benchmarks/grounded_query/golden_graph_multihop_suite.json"

benchmark-grounded-technical:
	@$(MAKE) benchmark-grounded IRONRAG_BENCHMARK_SUITES="apps/api/benchmarks/grounded_query/golden_technical_contract_suite.json" IRONRAG_BENCHMARK_OUTPUT_DIR="tmp-technical-benchmarks" IRONRAG_BENCHMARK_LIBRARY_NAME="Technical Benchmark"

benchmark-grounded-technical-seed:
	@$(MAKE) benchmark-grounded-seed IRONRAG_BENCHMARK_SUITES="apps/api/benchmarks/grounded_query/golden_technical_contract_suite.json" IRONRAG_BENCHMARK_OUTPUT_DIR="tmp-technical-benchmarks" IRONRAG_BENCHMARK_LIBRARY_NAME="Technical Benchmark Seed"

benchmark-golden:
	@$(MAKE) benchmark-grounded IRONRAG_BENCHMARK_SUITES="$(IRONRAG_GOLDEN_SUITES)" IRONRAG_BENCHMARK_OUTPUT_DIR="$(IRONRAG_GOLDEN_OUTPUT_DIR)" IRONRAG_BENCHMARK_LIBRARY_NAME="Golden Benchmark"

benchmark-golden-seed:
	@$(MAKE) benchmark-grounded-seed IRONRAG_BENCHMARK_SUITES="$(IRONRAG_GOLDEN_SUITES)" IRONRAG_BENCHMARK_OUTPUT_DIR="$(IRONRAG_GOLDEN_OUTPUT_DIR)" IRONRAG_BENCHMARK_LIBRARY_NAME="Golden Benchmark Seed"

benchmark-contract-test:
	python3 -m unittest discover -s apps/api/benchmarks/grounded_query -p 'test_*.py'
	python3 -m unittest discover -s scripts/bench -p 'test_*.py'

benchmark-regression:
	@test -n "$(IRONRAG_BENCHMARK_BASELINE_DIR)" || (echo "IRONRAG_BENCHMARK_BASELINE_DIR is required" && exit 1)
	@test -n "$(IRONRAG_BENCHMARK_CANDIDATE_DIR)" || (echo "IRONRAG_BENCHMARK_CANDIDATE_DIR is required" && exit 1)
	python3 apps/api/benchmarks/grounded_query/compare_benchmarks.py \
		"$(IRONRAG_BENCHMARK_BASELINE_DIR)" \
		"$(IRONRAG_BENCHMARK_CANDIDATE_DIR)" \
		--max-latency-regression-percent "$(IRONRAG_BENCHMARK_MAX_LATENCY_REGRESSION_PERCENT)"

docker-local-build:
	$(LOCAL_IMAGE_ENV) $(DOCKER_COMPOSE) -f $(DOCKER_COMPOSE_FILE) build $(LOCAL_DOCKER_BUILD_SERVICES)

docker-local-rebuild:
	$(LOCAL_IMAGE_ENV) $(DOCKER_COMPOSE) -f $(DOCKER_COMPOSE_FILE) build --no-cache $(LOCAL_DOCKER_BUILD_SERVICES)

docker-local-redeploy:
	$(LOCAL_IMAGE_ENV) $(DOCKER_COMPOSE) -f $(DOCKER_COMPOSE_FILE) stop $(LOCAL_DOCKER_STEADY_SERVICES)
	$(LOCAL_IMAGE_ENV) $(DOCKER_COMPOSE) -f $(DOCKER_COMPOSE_FILE) up -d --no-deps --no-build --pull never --force-recreate startup
	@sid="$$( $(LOCAL_IMAGE_ENV) $(DOCKER_COMPOSE) -f $(DOCKER_COMPOSE_FILE) ps -aq startup )"; \
		test -n "$$sid"; \
		test "$$( $(DOCKER) wait "$$sid" )" = 0; \
		test "$$( $(DOCKER) inspect -f '{{.State.ExitCode}}' "$$sid" )" = 0
	$(LOCAL_IMAGE_ENV) $(DOCKER_COMPOSE) -f $(DOCKER_COMPOSE_FILE) up -d --no-deps --no-build --pull never --force-recreate --wait --wait-timeout 300 $(LOCAL_DOCKER_STEADY_SERVICES)

docker-local-refresh:
	$(MAKE) docker-local-build
	$(MAKE) docker-local-redeploy

docker-local-up:
	$(LOCAL_IMAGE_ENV) $(DOCKER_COMPOSE) -f $(DOCKER_COMPOSE_FILE) up -d $(LOCAL_DOCKER_ALL_SERVICES)

docker-local-down:
	$(LOCAL_IMAGE_ENV) $(DOCKER_COMPOSE) -f $(DOCKER_COMPOSE_FILE) down

perf-probe:
	python3 scripts/ops/profile-ui-endpoints.py

agent-perf-probe:
	@test -n "$(IRONRAG_AGENT_PROBE_LIBRARY_ID)" || (echo "IRONRAG_AGENT_PROBE_LIBRARY_ID is required" && exit 1)
	@test -n "$(IRONRAG_PROBE_PASSWORD)" || (echo "IRONRAG_PROBE_PASSWORD is required" && exit 1)
	@set -- \
	  --base-url "$(or $(IRONRAG_AGENT_PROBE_BASE_URL),http://127.0.0.1:19000)" \
	  --login "$(or $(IRONRAG_AGENT_PROBE_LOGIN),admin)" \
	  --library-id "$(IRONRAG_AGENT_PROBE_LIBRARY_ID)"; \
	if [ -n "$(IRONRAG_AGENT_PROBE_WORKSPACE_ID)" ]; then \
	  set -- "$$@" --workspace-id "$(IRONRAG_AGENT_PROBE_WORKSPACE_ID)"; \
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
	if [ -n "$(IRONRAG_AGENT_PROBE_ASSISTANT_RUNS)" ]; then \
	  set -- "$$@" --assistant-runs "$(IRONRAG_AGENT_PROBE_ASSISTANT_RUNS)"; \
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
	if [ -n "$(IRONRAG_AGENT_PROBE_EXPECTED_SEARCH_TOP_LABEL)" ]; then \
	  set -- "$$@" --expected-search-top-label "$(IRONRAG_AGENT_PROBE_EXPECTED_SEARCH_TOP_LABEL)"; \
	fi; \
	if [ -n "$(IRONRAG_AGENT_PROBE_MAX_TOOL_LATENCY_MS)" ]; then \
	  set -- "$$@" --max-tool-latency-ms "$(IRONRAG_AGENT_PROBE_MAX_TOOL_LATENCY_MS)"; \
	fi; \
	if [ -n "$(IRONRAG_AGENT_PROBE_MAX_COMPLETED_MS)" ]; then \
	  set -- "$$@" --max-completed-ms "$(IRONRAG_AGENT_PROBE_MAX_COMPLETED_MS)"; \
	fi; \
	python3 scripts/ops/profile-agent-surfaces.py "$$@"

agent-perf-suite:
	@test -n "$(IRONRAG_AGENT_PROBE_LIBRARY_ID)" || (echo "IRONRAG_AGENT_PROBE_LIBRARY_ID is required" && exit 1)
	@test -n "$(IRONRAG_PROBE_PASSWORD)" || (echo "IRONRAG_PROBE_PASSWORD is required" && exit 1)
	@set -- \
	  --suite-path "$(or $(IRONRAG_AGENT_PROBE_SUITE_PATH),scripts/ops/agent-surface-suite.json)" \
	  --base-url "$(or $(IRONRAG_AGENT_PROBE_BASE_URL),http://127.0.0.1:19000)" \
	  --login "$(or $(IRONRAG_AGENT_PROBE_LOGIN),admin)" \
	  --library-id "$(IRONRAG_AGENT_PROBE_LIBRARY_ID)"; \
	if [ -n "$(IRONRAG_AGENT_PROBE_WORKSPACE_ID)" ]; then \
	  set -- "$$@" --workspace-id "$(IRONRAG_AGENT_PROBE_WORKSPACE_ID)"; \
	fi; \
	if [ -n "$(IRONRAG_AGENT_PROBE_REPORTS_DIR)" ]; then \
	  set -- "$$@" --reports-dir "$(IRONRAG_AGENT_PROBE_REPORTS_DIR)"; \
	fi; \
	if [ -n "$(IRONRAG_AGENT_PROBE_SUITE_OUTPUT_PATH)" ]; then \
	  set -- "$$@" --output-path "$(IRONRAG_AGENT_PROBE_SUITE_OUTPUT_PATH)"; \
	fi; \
	python3 scripts/ops/run-agent-surface-suite.py "$$@"
