# Relay Profile: Standard Interconnect

The standard interconnect profile, registered as RLY-STD-204, is the
default relay configuration for rings that carry mixed tenant traffic.

## Parameters

- Registration id: RLY-STD-204
- Frame window: 64 frames
- Acknowledgement mode: cumulative
- Redundancy: single spare relay per ring
- Failover deadline: 800 ms

## Operational notes

RLY-STD-204 favors throughput over failover speed. The cumulative
acknowledgement mode batches confirmations, which keeps the control channel
quiet but delays loss detection by up to one full frame window. Rings that
host latency-sensitive tenants should evaluate the extended profile
instead of tuning RLY-STD-204 beyond its registered parameters.

Changing the frame window of a registered profile is not permitted; a
different window requires registering a new profile id.
