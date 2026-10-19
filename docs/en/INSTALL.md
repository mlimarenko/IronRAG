<div align="center">

# Install and bootstrap

### Interactive and unattended installation, provider keys, telemetry

[Overview](./README.md) | [Russian](../ru/INSTALL.md) | [AI bindings](./AI-BINDINGS.md) | [Credential encryption](./CREDENTIAL-ENCRYPTION.md) | [Capacity planning](./CAPACITY-PLANNING.md)

</div>

## Interactive install

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

Run `./install.sh --help` for the full flag and environment-variable list, or
`--plan-only` to print the detected profile without writing or deploying
anything.

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
