# Relay Profile: Extended Interconnect

The extended interconnect profile, registered as RLY-EXT-517, is the relay
configuration for rings dedicated to latency-sensitive tenants.

## Parameters

- Registration id: RLY-EXT-517
- Frame window: 16 frames
- Acknowledgement mode: immediate
- Redundancy: dual spare relays per ring
- Failover deadline: 200 ms

## Operational notes

RLY-EXT-517 trades control-channel chatter for fast loss detection. The
immediate acknowledgement mode confirms every frame, so the control channel
carries roughly four times the confirmation traffic of the standard
profile. The dual spare relays allow one spare to be drained for
maintenance while the ring keeps its failover guarantee.

Changing the frame window of a registered profile is not permitted; a
different window requires registering a new profile id.
