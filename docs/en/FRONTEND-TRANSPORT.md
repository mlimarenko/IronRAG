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

## Deployment branding

The frontend reads **`/branding.json` before rendering**. The shipped file is
`{}`: omitted settings retain the IronRAG display name and bundled mark. This is
runtime configuration, not a `VITE_*` build variable; the same frontend image
can serve different deployment brands without rebuilding.

```json
{
  "displayName": "Team Knowledge",
  "logoUrl": "/brand/logo.svg",
  "iconUrl": "/brand/icon.svg",
  "faviconUrl": "/brand/favicon.png"
}
```

`displayName` is plain text (1–120 characters after trimming). `logoUrl` is the
full-size mark, `iconUrl` the compact mark, and `faviconUrl` the browser icon.
An omitted icon uses the logo; an omitted favicon uses the icon. The display
name also sets the browser title and Open Graph title. There is no HTML injection
or operator-supplied CSS. Unknown fields, including permission settings, are
rejected: branding does **not** change roles, access controls, or navigation
permissions.

Assets must be root-relative paths or absolute HTTP(S) URLs on the **same
origin** (including protocol and port). External and protocol-relative URLs,
embedded credentials, `javascript:`, `data:`, `blob:`, and malformed URLs are
rejected. Serve custom assets from the frontend; its existing CSP stays intact.

Create `branding.json` and a `brand/` directory beside your Compose file, then
add these read-only mounts in `compose.override.yaml`:

```yaml
services:
  frontend:
    volumes:
      - ./branding.json:/usr/share/nginx/html/branding.json:ro
      - ./brand:/usr/share/nginx/html/brand:ro
```

Run `docker compose up -d frontend`, then reload the browser. Only the frontend
needs these mounts. The existing Docker build copies the default JSON with the
other public assets; no image environment variables or backend changes are
needed. In Kubernetes, mount a ConfigMap file at the same JSON path and serve
the assets from the same origin. Configuration is fetched with `no-store`;
publish config and assets together, and use revisioned asset filenames when
changing images. The config request has a five-second timeout. Invalid/unavailable
JSON blocks startup with the existing
localized error screen and a console message naming `/branding.json`, rather
than silently discarding operator settings. Correct the file and reload.

For local Vite development, edit `apps/web/public/branding.json` and place assets
under `apps/web/public/brand/`; do not commit deployment-specific files. Components
consume `getBranding().displayName` from `@/app/branding` and `Brand` / `BrandMark`
from `@/app/Brand`. `BrandMark` accepts `compact`, `size`, and `className`; it is
decorative, so callers displaying it alone must provide an accessible label.

## Related docs

- [README — quick start](../../README.md#quick-start) — default `http://127.0.0.1:19000`.
