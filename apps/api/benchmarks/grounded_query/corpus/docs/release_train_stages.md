# Release Train: Stage Sequence

The Orbita release train promotes every build through a fixed sequence of
stages. A build may never skip a stage, and a stage may never be re-entered
after the train has moved past it.

## Stage order

1. compile-and-gate
2. sealed-rehearsal
3. canary-slice
4. broad-rollout
5. ledger-close

The sealed-rehearsal stage replays the previous week of recorded traffic
against the candidate build inside an isolated ring. Only after
sealed-rehearsal succeeds does the train enter the canary-slice stage,
which routes 4 percent of live traffic to the candidate.

The broad-rollout stage expands the candidate to all rings over 6 hours.
The final ledger-close stage freezes the release record and emits the
signed manifest.

## Rollback rule

A rollback is permitted only from the canary-slice and broad-rollout
stages. Once ledger-close has completed, the train can move forward only:
a defect found later requires a new train, never a rewind.
