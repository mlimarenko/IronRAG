<div align="center">

# Credential encryption operations

### Mandatory encryption and master-key rotation

[Overview](./README.md) | [Russian](../ru/CREDENTIAL-ENCRYPTION.md) | [Webhooks](./WEBHOOK.md) | [AI bindings](./AI-BINDINGS.md)

</div>

## Safety model

IronRAG stores AI account credentials, outbound webhook signing secrets, and
webhook custom-header values in authenticated, row-bound `ironrag:enc:v3`
envelopes. The master key must be kept outside PostgreSQL and backed up
separately.

Credential encryption is **mandatory and always on**:

- `IRONRAG_CREDENTIAL_MASTER_KEY` and its keyring are required. The API and
  workers **refuse to start** without a valid key (fail closed); there is no
  disabled or read-only mode.
- Every stored credential is a v3 envelope. A stored value that is not a v3 envelope
  fails closed: it is rejected on read and reported as invalid by the migration
  inventory rather than decrypted.
- Reads succeed for the active v3 key and any configured previous v3 keys. An
  unknown v3 key ID fails closed.

`install.sh` generates a master key when it creates a fresh `.env`, and
adds one to an existing `.env` without the key. Docker Compose and
Helm require the key to be supplied (inline or through an external Secret).

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
  restartNonce: "credential-rotation-phase-1-2026-07-10"
```

Use a new nonce for each key-rotation phase.

## Three-phase master-key rotation

Key IDs contain 1-32 lowercase letters, digits, `.`, `_`, or `-` and start with
an alphanumeric character. `IRONRAG_CREDENTIAL_PREVIOUS_MASTER_KEYS` accepts up
to eight unique `id=padded-standard-base64-key` entries with no whitespace,
strictly sorted by key ID. A previous entry is decrypt/rewrap-only.

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

- the final inventory reports no remaining old-key rows;
- every API and worker uses the new active key;
- database backups containing old-key envelopes have aged out according to the
  retention policy; and
- a restore drill confirms the retained backups and retained key material match.

Then remove the old entry, roll the reduced keyring to every replica, and verify
readiness again. Never delete the only copy of a retired key immediately; keep
it under the approved backup-retention and destruction policy.
