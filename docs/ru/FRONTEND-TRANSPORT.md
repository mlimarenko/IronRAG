# Транспорт frontend

Дефолтный Compose поднимает UI по **HTTP** на порту `19000`. Это удобно,
когда TLS завершается на ingress или reverse proxy (с `proxy_buffering off`
для SSE на маршрутах ассистента).

Образ frontend может отдавать **TLS напрямую**: смонтируйте сертификат и ключ
в `/etc/nginx/tls/tls.crt` и `/etc/nginx/tls/tls.key`, опубликуйте `443/tcp` и
`443/udp`. При наличии файлов nginx включает HTTP/2, объявляет HTTP/3 over QUIC
через `Alt-Svc`, а проксирование API оставляет на HTTP/1.1 — так стабильнее
стриминг ответов и крупные snapshot upload'ы.

## Пример Compose

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

## Чеклист reverse proxy

Когда TLS завершается перед контейнером frontend:

- `proxy_buffering off` и `proxy_request_buffering off` на SSE-маршрутах ассистента.
- Сохраняйте заголовки `Upgrade` / `Connection`, если позже добавите WebSocket.
- Upstream — на frontend-сервис (`IRONRAG_API_UPSTREAM` внутри контейнера по-прежнему
  указывает на backend для `/v1/*`).

## Связанные документы

- [Архитектура frontend](./FRONTEND.md)
- [README — quick start](../../README.md#quick-start)
