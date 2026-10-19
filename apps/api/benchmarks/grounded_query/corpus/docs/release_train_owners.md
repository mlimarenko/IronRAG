# Release Train: Stage Ownership

Each stage of the Orbita release train has exactly one owning team. The
owner is accountable for the stage gate decision and is the only team that
may approve an exception.

## Ownership table

| Stage | Owning team | Escalation window |
|---|---|---|
| compile-and-gate | Build Systems | 30 minutes |
| sealed-rehearsal | Replay Guild | 2 hours |
| canary-slice | Traffic Stewards | 45 minutes |
| broad-rollout | Traffic Stewards | 45 minutes |
| ledger-close | Release Registrars | 24 hours |

The Traffic Stewards own both live-traffic stages, so a single pager
rotation covers the riskiest part of the train. The Release Registrars own
ledger-close alone; their 24 hour escalation window exists because manifest
signing involves an offline key ceremony.

## Exception policy

An exception request must name the stage, the owning team, and the exact
gate check being waived. Exceptions are logged into the train ledger and
expire with the train: they never carry over to the next release.
