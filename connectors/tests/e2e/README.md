# Local Docker E2E

The opt-in suites use a real pinned YouTrack Server. The full-stack suite also
uses the immutable IronRAG v0.5.9 images, its PostgreSQL/Redis/worker services,
the built connector image, and a deterministic local OpenAI-compatible
provider. No external model or production service is called.

The lifecycle crosses three YouTrack REST pages and verifies create, no-op,
rename/body update, empty-body replacement, creation, deletion, connector
restart with the same SQLite volume, IronRAG source revisions, worker-produced
knowledge chunks, search, soft deletion, and final reaping.

All published ports bind to loopback. The commands below use unique Compose
project names and cleanup only those projects. Do not replace the scoped
`docker compose down` commands with a global Docker prune.

## Prerequisites

- Docker with Compose v2.
- Python 3.12 and `uv`.
- Node.js, Playwright, and Chromium for the one-time YouTrack wizard.
- A clean IronRAG source checkout at tag `v0.5.9` for its Compose file.
- Nothing to check out: the framework and every source are in this tree.

Install the browser once if needed:

```bash
npm install -g playwright
playwright install chromium
```

## 1. Start IronRAG and YouTrack

From the connector repository root, point `IRONRAG_SOURCE` at the clean
IronRAG v0.5.9 checkout:

```bash
export IRONRAG_SOURCE=/absolute/path/to/IronRAG-v0.5.9
export IRONRAG_BOOTSTRAP_ENV=/tmp/ironrag-youtrack-full-e2e.env
export YOUTRACK_E2E_ENV_FILE=/tmp/ironrag-youtrack-e2e.env

IRONRAG_E2E_ENV_FILE="$IRONRAG_BOOTSTRAP_ENV" \
  node tests/e2e/bootstrap_ironrag.cjs

docker compose -p ironrag-youtrack-full-e2e \
  --env-file "$IRONRAG_BOOTSTRAP_ENV" \
  -f "$IRONRAG_SOURCE/docker-compose.yml" \
  up -d --pull missing --no-build --wait --wait-timeout 300 \
  postgres redis startup backend worker frontend

docker compose -p ironrag-youtrack-e2e \
  -f tests/e2e/docker-compose.yml \
  up -d --pull missing --wait --wait-timeout 300

YOUTRACK_E2E_ENV_FILE="$YOUTRACK_E2E_ENV_FILE" \
  node tests/e2e/bootstrap_youtrack.cjs
```

Both bootstrap files are created atomically with mode `0600`. The scripts do
not print the generated passwords or API tokens.

## 2. Provision the mock provider and scoped IronRAG runtime

Start only the provider first. The external network names are deliberately
tied to the two isolated projects above:

```bash
export FULL_STACK_COMPOSE=tests/e2e/docker-compose.full.yml
export CONNECTOR_E2E_ENV_FILE=/tmp/ironrag-youtrack-connector-e2e.env
export CONNECTOR_E2E_ROUTING_FILE=/tmp/ironrag-youtrack-connector-e2e-routing.yaml
export CONNECTOR_E2E_HOST_ENV_FILE=/tmp/ironrag-youtrack-connector-e2e-host.env

docker compose -p ironrag-youtrack-connector-e2e \
  -f "$FULL_STACK_COMPOSE" \
  up -d --build --wait mock-provider

set -a
source "$IRONRAG_BOOTSTRAP_ENV"
set +a
export IRONRAG_E2E_MOCK_PROVIDER_URL=http://mock-provider:8080/v1
export IRONRAG_E2E_RUNTIME_FILE=/tmp/ironrag-youtrack-full-e2e.runtime.json
uv run python tests/e2e/bootstrap_full_stack.py

set -a
source "$YOUTRACK_E2E_ENV_FILE"
set +a
uv run python tests/e2e/prepare_connector_runtime.py
```

`bootstrap_full_stack.py` creates an isolated library, configures its five
Ollama-profile bindings against the deterministic provider, and mints a
library-scoped token with only `library_read` and `library_write` permissions.
Its six-key runtime artifact carries the service URL and token, the workspace
and library slugs used for the friendly routing reference, and the internal
workspace and library IDs retained for IAM and lifecycle assertions.
`prepare_connector_runtime.py` converts that runtime into connector, routing,
and host-test files without exposing secrets. The generated routing target is
`<workspace-slug>/<library-slug>`; no UUID appears in routing configuration.
The two secret-bearing environment files use mode `0600`; the non-secret
routing file uses mode `0644` so container UID 10001 can read the bind mount.

## 3. Build and test the connector container

Stage the exact ConnectorTemplate v0.1.0 release in the ignored Docker build
directory, then start the connector on both isolated Docker networks:

```bash
set -a
source "$CONNECTOR_E2E_HOST_ENV_FILE"
set +a
docker compose -p ironrag-youtrack-connector-e2e \
  --env-file "$CONNECTOR_E2E_HOST_ENV_FILE" \
  -f "$FULL_STACK_COMPOSE" \
  up -d --build --wait connector

set -a
source "$YOUTRACK_E2E_ENV_FILE"
source "$CONNECTOR_E2E_HOST_ENV_FILE"
set +a
uv run pytest -vv -m live tests/e2e
```

The full-stack test removes its synthetic articles from both YouTrack and the
active IronRAG catalog before returning. The Docker volumes remain available
for inspection until the explicit cleanup below.

## Real YouTrack only

To run the faster adapter lifecycle with an in-memory IronRAG double:

```bash
set -a
source "$YOUTRACK_E2E_ENV_FILE"
set +a
uv run pytest -vv -m live tests/e2e/test_live_lifecycle.py
```

Destructive YouTrack tests default to loopback. A deliberately isolated remote
fixture additionally requires `YOUTRACK_E2E_ALLOW_REMOTE_DESTRUCTIVE=1`.

## Cleanup

```bash
docker compose -p ironrag-youtrack-connector-e2e \
  --env-file "$CONNECTOR_E2E_HOST_ENV_FILE" \
  -f "$FULL_STACK_COMPOSE" \
  down -v --remove-orphans --timeout 120

docker compose -p ironrag-youtrack-e2e \
  -f tests/e2e/docker-compose.yml \
  down -v --remove-orphans --timeout 120

docker compose -p ironrag-youtrack-full-e2e \
  --env-file "$IRONRAG_BOOTSTRAP_ENV" \
  -f "$IRONRAG_SOURCE/docker-compose.yml" \
  down -v --remove-orphans --timeout 120


rm -f "$IRONRAG_BOOTSTRAP_ENV" "$YOUTRACK_E2E_ENV_FILE" \
  "$IRONRAG_E2E_RUNTIME_FILE" "$CONNECTOR_E2E_ENV_FILE" \
  "$CONNECTOR_E2E_ROUTING_FILE" "$CONNECTOR_E2E_HOST_ENV_FILE"
```
