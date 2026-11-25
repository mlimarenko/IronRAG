<div align="center">

# Install and bootstrap

### Interactive and unattended installation, provider keys, telemetry

[Overview](./README.md) | [Russian](../ru/INSTALL.md) | [AI bindings](./AI-BINDINGS.md) | [Credential encryption](./CREDENTIAL-ENCRYPTION.md) | [Capacity planning](./CAPACITY-PLANNING.md)

</div>

## Interactive install

For an existing v0.6.3 installation, follow the
[v0.6.4 upgrade procedure](#upgrade-from-v063-to-v064) before starting the new
images or re-running the installer.

```bash
curl -fsSL https://raw.githubusercontent.com/mlimarenko/IronRAG/master/install.sh | bash
```

The installer is an interactive wizard: it inspects the host (CPU + RAM),
recommends a resource profile (per-service memory/CPU caps and ingest
parallelism), and prompts step by step for the port and optional admin
bootstrap, then shows a review screen before writing anything. Provider keys
come from the typed environment namespace below or are configured in the UI;
the installer carries no provider-name table. On a re-run it preserves the
existing `.env` secrets and tuned caps, while official IronRAG image pins are
advanced to the selected release tag.

Image text recognition uses the configured `extract_text` LLM Vision binding in
every resource profile. Xberg handles native text/structure and rendering; no
profile installs a local OCR model. Configure a Vision-capable binding under
**Admin → AI → Bindings** before ingesting images or scanned pages.

The installer recommends `micro` below 8 GiB, `small` below 16 GiB, `medium`
below 32 GiB, and `large` otherwise. Worker limits are 768 MiB, 1536 MiB,
3072 MiB and 5120 MiB respectively. It checks the steady-state cap sum with
max(512 MiB, 12% of host RAM) reserved for the host. Compose defaults to a
3072 MiB worker; Helm starts one worker with request 768 MiB and limit 3 GiB.
Native rendering and image encoding still consume bounded RAM; these defaults
are not a universal workload capacity or recognition quality guarantee.

For a pinned existing resource plan, use `--recompute-resources` to apply the
worker limits. Otherwise existing secrets and manually pinned caps are kept.
The backend image builds `ironrag-extract` with the locked Cargo graph; image
text recognition runs through the remote Vision provider, not bundled models.

Run `./install.sh --help` for the full flag and environment-variable list, or
`--plan-only` to print the detected profile without writing or deploying
anything.

## Upgrade bundled PostgreSQL to 19

New bundled installations use PostgreSQL 19 with pgvector 0.8.7. An existing
PostgreSQL 18 volume needs a database migration, not an image swap. Keep the old
volume and image. Do not change `PGDATA` to point PostgreSQL 19 at the old files,
and do not delete the old volume to get past the startup refusal.

1. Rehearse a logical dump and restore to a separate PostgreSQL 19 cluster.
   Check the database encoding and locale, extensions, roles and grants, schema
   migration ledger, vector indexes, and representative records. Measure the
   maintenance window on a copy of the database. Keep the existing content store,
   encryption key, and database credentials.
2. Stop and drain API, workers, connectors, and any scheduled writer. Take and
   verify a fresh recoverable backup. Use `pg_dump` in custom format for the
   database and preserve role definitions separately if the deployment needs
   them. PostgreSQL role dumps can contain password hashes; keep them with the
   protected backup. Preserve the contents and configuration of the old volume.
3. Initialize PostgreSQL 19 on a **new** physical volume and restore with
   `pg_restore --exit-on-error`. In Compose, keep the same project and select
   the new volume with `IRONRAG_POSTGRES_VOLUME_NAME`; start only `postgres`
   with `docker compose up -d --no-deps postgres`. In Helm, prepare the new
   cluster on a separate PVC that is not behind the application database
   Service; set `dependencies.postgres.persistence.existingClaim` to that
   claim at cutover. Do not let the normal application rollout run before the
   restore is complete.
4. Confirm the server and extension versions, schema ledger, counts, grants,
   and vector search on the restored database. Bring up the application against
   the new volume and check readiness before reopening traffic. The old volume
   is a rollback point only until the new cluster accepts writes; after that,
   do not switch back to it and discard the new data.

For externally managed PostgreSQL, use the database provider's major-upgrade
procedure and verify pgvector before changing the IronRAG connection.

## Upgrade from v0.6.3 to v0.6.4

This upgrade requires a maintenance window and the guarded
[`text-publication-schema` transition](./CLI.md#migrate-text-publication-schema).
That reference owns the exact predecessor, refusal conditions and schema behavior.
Do not run the new startup service against the old schema or edit the SQLx ledger
to bypass a refusal.

Prepare the published v0.6.4 Compose definition or Helm chart and the verified
release digests for backend, frontend and every enabled bundled connector.
The migration and the replacement startup, API and worker must use the **same
backend digest**. The released executable is
`/usr/local/bin/ironrag-maintenance`, not `ironrag-cli`; it reads the backend
environment, including `IRONRAG_DATABASE_URL`.

Preserve the existing deployment identity and configuration: Compose project or
Helm release/namespace, PostgreSQL database and volume/PVC, vector extension and
data, content volume or bucket, Secrets and `IRONRAG_CREDENTIAL_MASTER_KEY`.
Do not regenerate keys, recreate storage or upgrade PostgreSQL, Redis or the
vector extension as part of this procedure. Pin stateful dependencies to the
installation's existing images when adopting the release configuration.

Before either dry-run or apply, block new requests and connector intake, let
in-flight work finish, and stop **all** old API, worker and maintenance writers.
Include separately deployed connectors, scheduled jobs and other replicas.
Prevent supervisors, autoscalers or GitOps reconciliation from restarting them.
A stop command alone is not evidence that work drained. Confirm quiescence, then
take and verify a rollback backup of PostgreSQL and content storage, retaining
the matching keys, configuration and v0.6.3 images. Keep dependencies available
for maintenance without restarting the old application.

### Compose

Use the existing installation directory, project name and `.env`. Change only
the application pins; replace the placeholders with the verified v0.6.4 digests:

```dotenv
IRONRAG_BACKEND_IMAGE=pipingspace/ironrag-backend:v0.6.4@sha256:<backend-digest>
IRONRAG_FRONTEND_IMAGE=pipingspace/ironrag-frontend:v0.6.4@sha256:<frontend-digest>
```

The [Compose definition](../../docker-compose.yml) shares the backend pin and
runtime environment across `startup`, `backend` and `worker`. Validate and pull
without starting services:

```bash
docker compose config --quiet
docker compose pull startup backend worker frontend
```

After blocking intake and draining work, stop the application roles. Stop external
writers separately and complete the backup checks above before proceeding:

```bash
docker compose stop frontend backend worker startup
```

Run the preview from the replacement backend, reusing its environment and mounts.
`--no-deps` prevents Compose from starting the normal startup service or other
dependencies; `--entrypoint` selects the packaged maintenance executable:

```bash
docker compose run --rm --no-deps \
  --entrypoint /usr/local/bin/ironrag-maintenance backend \
  migrate text-publication-schema --json
```

Review the successful preview and backup before running the separate apply step.
Keep writers stopped and use the unchanged image pin and configuration:

```bash
docker compose run --rm --no-deps \
  --entrypoint /usr/local/bin/ironrag-maintenance backend \
  migrate text-publication-schema --apply --json
```

Only after successful apply, recreate the normal startup container:

```bash
docker compose up -d --no-deps --no-build --force-recreate startup
docker compose logs --tail=80 startup
docker compose ps -a startup
```

Wait for `startup` to exit with code 0. Logs or a running container are not enough.
Then replace the API, worker and frontend:

```bash
docker compose up -d --no-deps --no-build backend worker frontend
docker compose ps -a
```

Confirm API and worker readiness and frontend health. Replace all enabled
connector images with their verified v0.6.4 artifacts, preserving configuration,
before reopening intake. These commands do not replace stateful services or
remove volumes.

### Helm

Use the existing release name and namespace and a reviewed values file for the
published v0.6.4 chart. Preserve dependency image pins, Secrets, PVCs and storage
settings. Set `api.image.digest` and `worker.image.digest` to the same verified
backend `sha256:…`, and `web.image.digest` to the verified frontend digest.
Keep the matching repositories. A digest takes precedence over `image.tag`;
updating a tag while leaving an old digest does not replace an image.

The chart's [startup Job](../../charts/ironrag/templates/startup-job.yaml) uses
`api.image`; API and worker init containers wait for that release revision's Job.
The chart does **not** provide a migration-command value. Its startup gate does
not stop old pods: API uses a rolling update. Do not use a normal `helm upgrade`
as the writer-drain or manual-migration step.

1. Block intake and pause GitOps reconciliation. Prevent HPA or other owners from
   restoring old replica counts, then scale the old API and worker deployments
   to zero using the installation's actual resource names. Stop other writers
   and confirm all old writer pods and jobs have terminated. Complete the backup
   checks above.
2. Prepare a separate one-off maintenance Job. To obtain the pod configuration,
   render only the startup Job locally, not Secrets. Here `RELEASE`, `NAMESPACE`
   and `VALUES` identify the existing installation and reviewed replacement
   values; run from the published v0.6.4 checkout:

   ```bash
   helm template "$RELEASE" ./charts/ironrag \
     --namespace "$NAMESPACE" -f "$VALUES" \
     --show-only templates/startup-job.yaml > maintenance-preview.yaml
   ```

   This requires `startup.mode=startup_job`. Give the Job a distinct name outside
   the chart's `*-startup-r<revision>` names and remove any Helm/Argo hook
   annotations. Set `spec.backoffLimit: 0` and the pod's `restartPolicy: Never`.
   Keep the pod's environment references, security context, mounts, pull secrets,
   dependency waits and network-policy labels. Check that its ConfigMap/Secret
   references already exist and select the intended database, that the backend
   image is the verified digest, and that its service account and network policy
   permit the required access. Do not apply the full rendered release here.
3. Override the `startup` container's command and arguments:

   ```yaml
   command: ["/usr/local/bin/ironrag-maintenance"]
   args: ["migrate", "text-publication-schema", "--json"]
   ```

   Create that reviewed Job in the existing namespace. Require successful
   completion and review its report. For apply, create a second distinctly named
   Job with the same image and pod configuration, adding `"--apply"` to `args`.
   Require successful completion; a failed or timed-out Job does not authorize
   starting application pods.
4. After apply, upgrade the existing release with the reviewed values and normal
   replica counts. Keep intake blocked until the new startup Job completes and
   API, worker and web are ready. Do not use automatic rollback to the old
   application after the schema transition:

   ```bash
   helm upgrade "$RELEASE" ./charts/ironrag \
     --namespace "$NAMESPACE" -f "$VALUES" \
     --wait --wait-for-jobs --timeout 20m
   ```

   Replace separately deployed connectors with the same release's verified
   images before resuming intake and reconciliation.

On a migration or startup refusal, keep writers stopped and inspect the reported
condition. After apply, reverting only application images is not a schema
rollback: the old binary rejects the new checksum. Returning to v0.6.3 requires
restoring the coordinated backup with its matching runtime. The transition does
not re-ingest sources, restore historical heads or enable financial scopes or
semantic rerank.

## Upgrade from v0.6.2 to v0.6.3

Keep the existing Compose project, `.env`, PostgreSQL volume, content-storage
volume or bucket, and `IRONRAG_CREDENTIAL_MASTER_KEY`. Take a backup before
upgrading. Do not recreate volumes or regenerate the key.

In the existing `.env`, change only the application image pins:

```dotenv
IRONRAG_BACKEND_IMAGE=pipingspace/ironrag-backend:v0.6.3
IRONRAG_FRONTEND_IMAGE=pipingspace/ironrag-frontend:v0.6.3
```

From that installation's directory, pull and replace the application services:

```bash
docker compose pull startup backend worker frontend
docker compose up -d startup backend worker frontend
docker compose ps -a
docker compose logs --tail=80 startup
```

The backend pin is shared by `startup`, `backend`, and `worker`. Compose runs
the new startup container before allowing the API and worker to start. For a
healthy public v0.6.2 installation, no SQL migration is needed: v0.6.3 has the
same baseline and checksum. Startup validates the existing schema automatically
and exits successfully without changing the migration ledger. An inconsistent
ledger or missing schema objects stop startup instead of being silently repaired.

Confirm that `startup` exited with code 0 and the application services are
healthy. These commands do not pull or replace PostgreSQL, Redis, or object
storage. No PostgreSQL major-version or vector-extension upgrade is part of
this procedure.

Update any separately deployed bundled connectors to their `v0.6.3` image tags,
preserving their configuration. With Helm, update the API and worker backend
images and the web image together, retain the existing Secrets and storage,
and let the chart's startup job complete before the application pods start.

Custom full-graph REST clients must replace
`GET /v1/knowledge/libraries/{libraryId}/graph` with the preparation flow in the
[release notes](../../CHANGELOG.md#063). The bundled frontend already uses it.

## From source

```bash
git clone https://github.com/mlimarenko/IronRAG.git
cd IronRAG
# First start only: the API and workers refuse to start without a credential
# master key, and .env.example ships it empty. Add
# IRONRAG_AI_PROVIDER_API_KEYS_JSON_B64 to the same file.
if [ ! -f .env ]; then
  sed "s|^IRONRAG_CREDENTIAL_MASTER_KEY=.*|IRONRAG_CREDENTIAL_MASTER_KEY=$(openssl rand -base64 32)|" \
    .env.example > .env
fi
docker compose up -d
```

Do not reuse this key across deployments, derive it from another secret, or lose
it: it decrypts the stored AI-account and webhook credentials
([Credential encryption](./CREDENTIAL-ENCRYPTION.md)). The
[interactive installer](#interactive-install) writes one for a fresh `.env` by
itself. Never overwrite an existing `.env`, and never regenerate its key while
the deployment's PostgreSQL volume persists: the stored credentials would stay
undecryptable. To change the port, resource profile, provider keys or bootstrap
admin on an existing install, re-run `install.sh`, which keeps the stored
secrets.

Open [http://127.0.0.1:19000](http://127.0.0.1:19000), create an admin account,
upload a document, and ask a question.

## Frontend branding

To change the deployment display name, logo, compact icon, and favicon without
rebuilding the frontend image, mount a runtime `branding.json` and same-origin
assets. See [Frontend transport — deployment branding](./FRONTEND-TRANSPORT.md#deployment-branding)
for the JSON contract and Compose mounts. The default remains IronRAG; branding
does not customize permissions or authentication.

## Unattended installation

With no terminal (the piped `curl | bash` form), or with `--yes` /
`--non-interactive`, the installer takes every answer from flags, environment
variables, and existing `.env` values. Answer precedence is flags > env >
prompt for non-secret values; secrets (admin password, provider API keys) are
accepted via environment variables or a pre-seeded `.env` only — never as
flags, since argv is visible to other processes and shell history.

```bash
curl -fsSL https://raw.githubusercontent.com/mlimarenko/IronRAG/master/install.sh -o install.sh
IRONRAG_PORT=8080 \
IRONRAG_PROFILE=small \
IRONRAG_AI_PROVIDER_API_KEYS_JSON_B64="$(printf '%s' '{"provider-alpha":"secret-value"}' | base64 | tr -d '\n')" \
  bash install.sh --non-interactive
```

## OpenID Connect SSO

IronRAG uses a confidential server-side Authorization Code client with PKCE.
Register one exact HTTPS callback at the identity provider and configure all of
these settings together. Leaving all five required variables empty disables SSO;
a partial configuration is rejected at startup.

```env
IRONRAG_OIDC_ISSUER_URL=https://id.example.com/realms/example
IRONRAG_OIDC_CLIENT_ID=ironrag
IRONRAG_OIDC_CLIENT_SECRET=<secret-manager-reference>
IRONRAG_OIDC_REDIRECT_URL=https://ironrag.example.com/v1/iam/oidc/callback
IRONRAG_OIDC_REQUIRED_GROUP=ironrag
```

For example, for a deployment at `https://ironrag.example.com`, register
precisely `https://ironrag.example.com/v1/iam/oidc/callback` as the client
redirect URI and include `https://ironrag.example.com` in
`IRONRAG_FRONTEND_ORIGIN`. Do not use wildcard redirects.

`IRONRAG_OIDC_CLIENT_SECRET` is a runtime credential. In a managed deployment,
store it in Vault or another secret manager and inject it as an environment
variable; do not commit it to `.env`, pass it on a command line, or bake it into
an image. The identity provider must include the configured required group in
the verified user claims. That group admits a user to IronRAG but does not
grant an IronRAG application role.

To make one SSO identity the first local IronRAG administrator, set the
optional `IRONRAG_OIDC_BOOTSTRAP_ADMIN_SUBJECT` to that identity provider's
verified immutable `sub` *before its first IronRAG login*. This grants the
local admin role only at first JIT provisioning. Every other admitted SSO user
starts as a local `viewer`; do not use email addresses to bootstrap or link
accounts. The role and token rules are described in [IAM.md](./IAM.md).

The separate break-glass password administrator remains available through
`IRONRAG_UI_BOOTSTRAP_ADMIN_LOGIN`, `IRONRAG_UI_BOOTSTRAP_ADMIN_EMAIL`, and
`IRONRAG_UI_BOOTSTRAP_ADMIN_PASSWORD`. Keep that password in the secret manager
as well. Startup reconciles it only for the exact configured local password
administrator, never for an SSO user.

## Provider API keys in `.env`

Set as many provider keys as you need in one JSON map, encode that map with
standard base64, and store the encoded value in `.env`:

```env
IRONRAG_CREDENTIAL_MASTER_KEY=<padded-standard-base64-of-32-random-bytes>
# Optional; defaults to `default`.
IRONRAG_CREDENTIAL_MASTER_KEY_ID=current-2026
# Configure only during rotation; key IDs must be sorted and unique.
IRONRAG_CREDENTIAL_PREVIOUS_MASTER_KEYS=old-2025=<base64-key>,old-2026=<base64-key>
IRONRAG_AI_PROVIDER_API_KEYS_JSON_B64=<standard-base64-of-compact-JSON-object>
```

Credentials auto-register on the next restart, and discovered catalog models
become selectable in the admin UI's binding profiles. Provider-kind
keys are preserved exactly without case folding or separator conversion, so a
new catalog provider does not require a code change.

The base64 envelope prevents dotenv/Compose from reinterpreting `$`, `#`,
quotes, backslashes, or Unicode inside a credential. Credential strings are
preserved byte-for-byte, including leading or trailing whitespace. The map is
bounded to 256 entries and 1 MiB of decoded JSON; each exact provider kind is
at most 128 UTF-8 bytes and each credential at most 64 KiB.

This convention replaces the former provider-specific
`IRONRAG_<PROVIDER>_API_KEY` variables. Move those entries into the JSON map
before upgrading; startup and `install.sh` reject malformed or removed names
instead of silently guessing an alias.

`IRONRAG_CREDENTIAL_MASTER_KEY` is mandatory at-rest encryption for stored
credentials. Fresh `install.sh` runs generate it automatically. For rotation,
see [CREDENTIAL-ENCRYPTION.md](./CREDENTIAL-ENCRYPTION.md).

## Telemetry

Fresh installations export nothing. Local structured logs stay available, while
OpenTelemetry export requires both `IRONRAG_OTEL_ENABLED=true` and an explicit
`OTEL_EXPORTER_OTLP_ENDPOINT` for a collector you control. A stable
`ironrag.deployment.id` is derived only after that opt-in. Log export remains
separately disabled because logs may contain document or query content.

Browser telemetry (Core Web Vitals, JavaScript errors, API failures, release
health and product events) is also off by default. It is baked into the frontend
image with `VITE_OTEL_EXPORTER_OTLP_ENDPOINT` (base URL; the app appends
`/v1/traces`, `/v1/metrics` and `/v1/logs`) and remains disabled until the viewer
grants consent in the UI. The same-origin `/otlp/` path is proxied by the web
container to `IRONRAG_OTLP_UPSTREAM`. Payloads are redacted in the browser: no
document content, questions, logins, emails, phone numbers or URL query strings
are exported, and the session identifier is random per tab.
