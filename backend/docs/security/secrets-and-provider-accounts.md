# Secrets and Provider Accounts

Provider credentials are first-class managed assets.

## Rules

- Never return raw secrets from list/detail APIs after creation.
- Keep metadata and secret payloads separate in API design.
- Redact secret-bearing config keys in logs, traces, and error payloads.
- Scope provider accounts to workspaces unless there is an explicit future requirement for instance-global credentials.

## Future Hardening

- dedicated secret encryption key management
- secret rotation workflows
- credential validation audit trail
- per-project provider allowlists
