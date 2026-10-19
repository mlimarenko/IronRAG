# Beacon Gateway Endpoint Matrix

The Beacon gateway terminates external traffic and forwards it to internal
services. Operators paste values from this matrix directly into probes and
firewall rules, so every entry is exact.

## Listener endpoints

| Purpose | Endpoint | Protocol |
|---|---|---|
| public API | edge-1.internal:8443 | HTTPS/2 |
| admin plane | edge-1.internal:9090 | HTTPS/1.1 |
| replication feed | relay-2.internal:7601 | mTLS stream |

The public API listener at edge-1.internal:8443 is the only endpoint
reachable from outside the perimeter. The replication feed on
relay-2.internal:7601 accepts connections only from the address block
10.32.0.0/19.

## Client compatibility

Gateway clients must satisfy the semver range >=2.4.1 <3.0.0 for the
beacon-client library. Clients outside that range are rejected during the
TLS handshake with alert code 116.

## Health probes

The liveness probe path is /healthz/live and the readiness probe path is
/healthz/ready. Both probes are served on the admin plane listener at
edge-1.internal:9090, never on the public API listener.
