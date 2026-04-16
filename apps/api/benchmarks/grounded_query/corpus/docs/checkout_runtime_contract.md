# Checkout Runtime Contract

The checkout server exposes a small REST surface for health and runtime metadata.

## System Information Endpoint

- Method: `GET`
- Path: `/system/info`
- Purpose: return the current checkout server system information, build metadata, and runtime status.
- Transport contract: JSON over HTTP.

Example response fields:

- `service`
- `version`
- `uptimeSeconds`
- `environment`

## Unsupported Transports

The checkout server does not publish a GraphQL API.

- No `/graphql` endpoint is exposed for this library.
- No GraphQL schema or GraphQL introspection contract is available.
- The canonical integration path for system information is the REST endpoint `GET /system/info`.
