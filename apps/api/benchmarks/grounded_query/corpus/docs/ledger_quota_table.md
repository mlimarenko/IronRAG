# Ledger Plan Quotas

Quotas below are enforced per billing month. The plan code is the only
stable identifier; display names may be rebranded without notice.

| Plan code | Display name | Documents | Queries per day | Spool retention | Export size |
|---|---|---|---|---|---|
| LP-010 | Seed | 200 | 150 | 6 hours | 50 MB |
| LP-020 | Sprout | 500 | 400 | 12 hours | 120 MB |
| LP-030 | Grove | 1500 | 900 | 24 hours | 300 MB |
| LP-040 | Orchard | 4000 | 2200 | 36 hours | 750 MB |
| LP-050 | Forest | 9000 | 5000 | 48 hours | 1.5 GB |
| LP-060 | Taiga | 20000 | 11000 | 72 hours | 4 GB |
| LP-070 | Biome | 45000 | 24000 | 96 hours | 9 GB |
| LP-080 | Sphere | 100000 | 52000 | 120 hours | 20 GB |

## Enforcement notes

Quota checks run at admission time. A request that would exceed the
queries-per-day quota is rejected with a retryable status and the reset
timestamp; the counter resets at 00:00 UTC. The export size quota applies
to a single export artifact, not to the monthly total. Only the LP-070 and
LP-080 plans may request a temporary export size raise, and only once per
billing month.
