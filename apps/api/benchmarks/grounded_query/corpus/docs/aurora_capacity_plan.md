# Aurora Platform Capacity Plan

This plan fixes the sizing envelope for the Aurora deployment rings. The
numbers below are contractual: alerting thresholds and the autoscaler
configuration are generated from this document.

## Tier sizing

The application tier scales between 3 and 12 nodes depending on request
pressure. The quorum tier is fixed at 5 nodes and must never be resized by
the autoscaler, because the consensus protocol requires a stable member set.
The ingest spool tier runs exactly 2 nodes in every ring.

| Tier | Nodes | Instance profile | Scaling |
|---|---|---|---|
| application | 3–12 | c7g.2xlarge | autoscaled |
| quorum | 5 | r7i.large | fixed |
| ingest spool | 2 | m7g.xlarge | fixed |

## Budgets

Every synchronous request carries an end-to-end latency budget of 250 ms,
measured at the edge. The quorum tier commit path has a stricter internal
budget of 40 ms. A full ring at peak load has a power draw of 42 kW, which
is the limit the colocation contract allows per cage.

## Storage envelope

The write-ahead spool retains 36 hours of traffic. At the observed peak of
18 MB/s of compressed writes this requires 2.4 TB of spool capacity per
node, provisioned as locally attached NVMe.

## Review cadence

Capacity numbers are reviewed every quarter. Any change to the quorum tier
size requires a consensus-safety review, because member-set changes force a
full lattice re-election.
