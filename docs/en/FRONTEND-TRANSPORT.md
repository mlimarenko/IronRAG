# Frontend transport

The default Compose stack exposes the web UI over **HTTP** on port `19000`.
That is the right shape when TLS terminates at an ingress or reverse proxy
(for example nginx on `example.com` with `proxy_buffering off` for SSE).

The frontend image can also serve **TLS directly**. Mount a certificate and key
at `/etc/nginx/tls/tls.crt` and `/etc/nginx/tls/tls.key`, then publish `443/tcp`
and `443/udp`. When those files exist, nginx enables HTTP/2, advertises HTTP/3
over QUIC with `Alt-Svc`, and keeps API proxying on HTTP/1.1 so streaming
responses and large snapshot uploads stay stable.

## Compose example

```yaml
services:
  frontend:
    ports:
      - "443:443/tcp"
      - "443:443/udp"
    volumes:
      - ./certs/fullchain.pem:/etc/nginx/tls/tls.crt:ro
      - ./certs/privkey.pem:/etc/nginx/tls/tls.key:ro
```

## Reverse-proxy checklist

When TLS terminates in front of the frontend container:

- Set `proxy_buffering off` and `proxy_request_buffering off` on assistant SSE routes.
- Preserve `Upgrade` / `Connection` headers if you add WebSocket paths later.
- Point upstream at the frontend service (`IRONRAG_API_UPSTREAM` inside the
  container still targets the backend for `/v1/*`).

## Related docs

- [Frontend architecture](./FRONTEND.md) — React app, streaming turns, inspector.
- [README — quick start](../../README.md#quick-start) — default `http://127.0.0.1:19000`.
