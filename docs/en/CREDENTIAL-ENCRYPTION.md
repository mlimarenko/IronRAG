<div align="center">

# Credential encryption operations

### Safe first upgrade, write-gate rollout, and master-key rotation

[Overview](./README.md) | [Russian](../ru/CREDENTIAL-ENCRYPTION.md) | [Webhooks](./WEBHOOK.md) | [AI bindings](./AI-BINDINGS.md)

</div>

## Safety model

IronRAG stores AI account credentials, outbound webhook signing secrets, and
webhook custom-header values in authenticated, row-bound `ironrag:enc:v3`
envelopes. The master key must be kept outside PostgreSQL and backed up
separately.

Two controls have different purposes:

- `IRONRAG_CREDENTIAL_MASTER_KEY` and its keyring let the current release read
  encrypted values.
- `IRONRAG_CREDENTIAL_ENCRYPTION_WRITE_ENABLED` permits it to create or update
  encrypted values. The default is `false`, so a mixed-version deployment
  cannot accidentally write ciphertext that an older process treats as
  plaintext.

Credential-bearing writes fail closed when the gate is disabled or the active
key is unavailable. Reads remain compatible with plaintext, legacy envelopes,
the active v3 key, and configured previous v3 keys. An unknown v3 key ID fails
closed.

`install.sh` enables encrypted writes only when it creates a fresh `.env`.
Docker Compose forwards the value but defaults it to `false`; Helm also defaults
to `false`.

## First upgrade of an existing installation

Do not combine the dual-reader deployment, enabling encrypted writes, and data
migration into one rolling update.

### 1. Prepare and deploy with writes disabled

1. Back up PostgreSQL and verify the restore procedure.
2. Generate 32 random bytes and encode them as canonical standard base64, for
   example with `openssl rand -base64 32 | tr -d '\n'`.
3. Store the key in the deployment secret manager, separately from the database
   password and database backup.
4. Distribute the active key and key ID to every API, worker, and startup
   process. Keep `IRONRAG_CREDENTIAL_ENCRYPTION_WRITE_ENABLED=false`.
5. Deploy the dual-reader release. Do not create, rotate, or edit provider or
   webhook credentials during this phase.

Older processes may still write plaintext while the rollout is in progress;
the dual-reader can read it. The new release cannot write v3 envelopes yet, so
older processes never receive unreadable ciphertext.

### 2. Verify every API and worker

Verify **every replica**, not one response through a load balancer:

- confirm each API and worker pod/container uses the intended immutable image
  digest;
- query `/v1/version` on each replica directly and confirm the version and
  service role;
- wait for every replica to become ready and check startup logs for keyring
  validation errors.

For Kubernetes, inspect image IDs for all runtime pods and port-forward to each
pod when checking `/v1/version`:

```bash
kubectl get pods -l app.kubernetes.io/name=ironrag \
  -o custom-columns='NAME:.metadata.name,ROLE:.metadata.labels.app\.kubernetes\.io/component,IMAGE_ID:.status.containerStatuses[0].imageID,READY:.status.containerStatuses[0].ready'

# Repeat for every API and worker pod.
kubectl port-forward pod/<pod-name> 18080:8080
curl --fail --silent http://127.0.0.1:18080/v1/version
```

### 3. Enable writes in a separate rollout

Set `IRONRAG_CREDENTIAL_ENCRYPTION_WRITE_ENABLED=true`, start a second rollout,
and again wait for every API and worker replica to restart and become ready.
Only then resume credential changes.

Do not roll back to a release that lacks the dual-reader after this point. If a
rollback is necessary, roll back only to a release that can read v3 envelopes
and keep every required key available.

### 4. Inventory and migrate

Run a dry inventory, apply the rewrap, and run the inventory again:

```bash
docker compose run --rm backend ironrag-maintenance migrate credential-secrets
docker compose run --rm backend ironrag-maintenance migrate credential-secrets --apply
docker compose run --rm backend ironrag-maintenance migrate credential-secrets
```

The inventory authenticates every encrypted value, including values that already
name the active key, and revalidates stored webhook headers. A malformed row is
counted with a bounded `(storage, id, error_code)` diagnostic while the scan
continues; secret values, URLs, headers, nonces, and ciphertext are never
reported. The command exits non-zero when any invalid value remains. Repair or
rotate those credentials and repeat the dry run until both invalid and rewrap counts
are zero.

Do not retire the legacy reader or any required key while the final inventory
or retained database backups still depend on it.

## Helm rollout behavior

For chart-rendered ConfigMaps and Secrets, checksum annotations automatically
restart API, worker, and startup pods whenever their runtime environment
changes.

Helm cannot checksum a Secret named by `runtimeSecret.existingSecret`, so a
non-empty `runtimeSecret.restartNonce` is required with it. Change the nonce on
every external Secret update. Reusing the nonce leaves running pods on their old
environment even if the external Secret object has changed.

```yaml
runtimeSecret:
  existingSecret: ironrag-runtime-production
  restartNonce: "credential-rollout-phase-1-2026-07-10"

app:
  credentialEncryptionWriteEnabled: false
```

Use a new nonce for the write-on rollout and for each key-rotation phase.

## Three-phase master-key rotation

Key IDs contain 1-32 lowercase letters, digits, `.`, `_`, or `-` and start with
an alphanumeric character. `IRONRAG_CREDENTIAL_PREVIOUS_MASTER_KEYS` accepts up
to eight unique `id=canonical-base64-key` entries with no whitespace, strictly
sorted by key ID. A previous entry is decrypt/rewrap-only.

Assume `key-2026-01` is active and `key-2026-07` is the new key.

### Phase 1: distribute the new key without switching

Keep the old key active. Put the new key in the previous-key map and roll this
bridging keyring to every API and worker:

```env
IRONRAG_CREDENTIAL_MASTER_KEY_ID=key-2026-01
IRONRAG_CREDENTIAL_MASTER_KEY=<old-key>
IRONRAG_CREDENTIAL_PREVIOUS_MASTER_KEYS=key-2026-07=<new-key>
```

Verify every replica has restarted successfully before proceeding. Although
the setting is named `previous`, using it for the new key in this phase is what
makes the next rolling overlap safe.

### Phase 2: switch the active key

Make the new key active and retain the old key as previous:

```env
IRONRAG_CREDENTIAL_MASTER_KEY_ID=key-2026-07
IRONRAG_CREDENTIAL_MASTER_KEY=<new-key>
IRONRAG_CREDENTIAL_PREVIOUS_MASTER_KEYS=key-2026-01=<old-key>
```

During this rollout, old-config replicas can decrypt new-key writes because
they received the new key in phase 1. New-config replicas can decrypt old-key
writes through the previous-key map.

### Phase 3: rewrap, verify, and retire

Run the dry/apply/dry migration sequence. Remove the old key only after:

- the final inventory reports no remaining plaintext, legacy, or old-key rows;
- every API and worker uses the new active key;
- database backups containing old-key envelopes have aged out according to the
  retention policy; and
- a restore drill confirms the retained backups and retained key material match.

Then remove the old entry, roll the reduced keyring to every replica, and verify
readiness again. Never delete the only copy of a retired key immediately; keep
it under the approved backup-retention and destruction policy.
