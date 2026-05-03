<div align="center">

# Webhooks IronRAG

### Исходящие webhooks: рассылка событий revision.ready и document.deleted подписчикам

[Обзор](./README.md) | [Webhooks (EN)](../en/WEBHOOK.md) | [MCP](./MCP.md) | [IAM](./IAM.md) | [CLI](./CLI.md)

</div>

## Обзор

IronRAG отправляет исходящие webhooks, уведомляя внешние системы об изменениях состояния. Приём входящих событий от vendor-систем (Confluence, MediaWiki, Notion и др.) — ответственность внешней middleware-прослойки, которая напрямую вызывает канонический HTTP API IronRAG (upload / replace / delete).

**Outbound** — `webhook_subscription` регистрирует HTTPS-эндпоинты, получающие HMAC-подписанные события об изменениях состояния IronRAG (`revision.ready`, `document.deleted`). Доставка durable: каждая отправка — `ingest_job` с `job_kind=webhook_delivery`, существующий пул воркеров обрабатывает lease/heartbeat/retry. На неудачу — экспоненциальный backoff до 8 попыток (cap 6 часов), затем `abandoned`.

## Модель подписки

Строки `webhook_subscription` описывают HTTP-приёмники событий IronRAG.

```
POST /v1/webhooks/subscriptions
Authorization: Bearer <api-token с workspace_admin>
Content-Type: application/json

{
  "workspace_id": "<uuid>",
  "library_id": "<uuid или null для workspace-wide>",
  "display_name": "Внутренний поисковый индекс",
  "target_url": "https://search.internal/ingest/ironrag-events",
  "secret": "<random 32+ байта hex>",
  "event_types": ["revision.ready", "document.deleted"],
  "custom_headers": {
    "X-Internal-Routing": "ironrag-prod"
  }
}
```

| Поле | Заметки |
|------|---------|
| `workspace_id` | Scope. Доставляются только события из этого workspace |
| `library_id` | Опционально. Если указан — только события из этой library; null = все libraries в workspace |
| `event_types` | Непустой массив канонических event names |
| `secret` | HMAC-SHA256 ключ для исходящих подписей |
| `custom_headers` | Свободные HTTP-заголовки, добавляемые к каждой отправке |
| `active` | По умолчанию `true`; `false` → подписка приостановлена |

CRUD-эндпоинты:

- `GET    /v1/webhooks/subscriptions?workspace_id=`
- `GET    /v1/webhooks/subscriptions/{id}`
- `POST   /v1/webhooks/subscriptions`
- `PATCH  /v1/webhooks/subscriptions/{id}`
- `DELETE /v1/webhooks/subscriptions/{id}`
- `GET    /v1/webhooks/subscriptions/{id}/attempts`

## Канонические события

### `revision.ready`

Срабатывает после того, как ingest-пайплайн закончил ревизию и продвинул её в readable. Шлётся на каждый успешный upload, replace, append или edit.

```json
{
  "event_type": "revision.ready",
  "event_id": "revision.ready:<revision_uuid>",
  "occurred_at": "2026-04-25T12:30:42Z",
  "workspace_id": "<uuid>",
  "library_id": "<uuid>",
  "document_id": "<uuid>",
  "revision_id": "<uuid>",
  "source_uri": "<display URL, если есть>"
}
```

### `document.deleted`

Срабатывает после коммита soft-delete и завершения post-commit cleanup.

```json
{
  "event_type": "document.deleted",
  "event_id": "document.deleted:<document_uuid>",
  "occurred_at": "2026-04-25T12:32:10Z",
  "workspace_id": "<uuid>",
  "library_id": "<uuid>",
  "document_id": "<uuid>"
}
```

## Схема исходящей подписи

Каждый исходящий POST несёт:

```
Content-Type: application/json
X-Ironrag-Signature: t=<unix_seconds>,v1=<hex_hmac_sha256>
X-Ironrag-Event-Type: revision.ready
X-Ironrag-Event-Id: revision.ready:<uuid>
```

Плюс любые `custom_headers` подписки.

Вход HMAC: `<ts_unix_seconds>.<raw байты тела>` — точка `.` буквальная. HMAC-ключ — `subscription.secret`.

### Верификация входящих событий (на стороне получателя)

```python
import hmac, hashlib, time

def verify(secret: bytes, header: str, body: bytes, window_seconds: int = 300) -> bool:
    try:
        parts = dict(p.split("=", 1) for p in header.split(","))
        ts = int(parts["t"])
        received_mac = parts["v1"]
    except (KeyError, ValueError):
        return False
    if abs(time.time() - ts) > window_seconds:
        return False  # окно replay превышено
    expected = hmac.new(secret, f"{ts}.".encode() + body, hashlib.sha256).hexdigest()
    return hmac.compare_digest(expected, received_mac)
```

**Не пересериализовывать тело** между получением и проверкой; байты должны совпадать byte-for-byte.

## Политика retry

| Результат | Поведение |
|-----------|-----------|
| HTTP 2xx | `delivered`, проставляется `delivered_at` |
| HTTP 5xx, 429, network/timeout | `attempt_number++`, `next_attempt_at = now + 2^min(attempt_number, 8)` минут (cap 6 ч). После 8 попыток → `abandoned` |
| HTTP 4xx (прочие) | `failed`, без retry |

Replay-защита: получатели ДОЛЖНЫ отклонять delivery с `t=` за пределами ±5 минут от своих часов.

## Детект изменений только-картинок

Когда PDF, DOCX или PPTX заменяет встроенную картинку без изменения OCR-текста, существующий `text_checksum` не изменился бы и канонический chunk-reuse plan пропустил бы re-embedding. Чтобы это исправить, IronRAG считает revision-level `image_checksum` (canonical sort всех байтов извлечённых картинок, затем SHA-256). Когда `parent.image_checksum != new.image_checksum`, chunk-reuse plan байпасится и embeddings + graph extraction пересчитываются полностью для этой ревизии. Семантика `text_checksum` сохранена (только текст).

## Операционные заметки

- **Секреты** хранятся в plaintext в `webhook_subscription.secret`. Encryption at rest на roadmap; сейчас БД — credentials store.
- **Job queue** общая с ingest-пайплайном. `job_kind=webhook_delivery` конкурирует с `content_mutation`, `web_discovery`, `web_materialize_page` за worker-leases. Тяжёлая outbound нагрузка может тормозить ingest; тюнить `IRONRAG_INGESTION_WORKER_POOL_SIZE`.
- **Наблюдаемость**: каждая outbound-попытка записана в `webhook_delivery_attempt` и запрашивается SQL для forensics. Воркеры эмитят `tracing` spans на стадии `webhook_delivery`.

## Reference: пример outbound

IronRAG эмитит `revision.ready` после завершения ingest документа. Подписчик передаёт событие в downstream поисковый индекс:

```python
import hmac, hashlib, time, json, requests

def verify_and_forward(secret: bytes, header: str, body: bytes):
    # Верифицировать подпись IronRAG
    parts = dict(p.split("=", 1) for p in header.split(","))
    ts = int(parts["t"])
    expected = hmac.new(secret, f"{ts}.".encode() + body, hashlib.sha256).hexdigest()
    assert hmac.compare_digest(expected, parts["v1"]), "bad signature"
    assert abs(time.time() - ts) < 300, "replay window exceeded"

    event = json.loads(body)
    if event["event_type"] == "revision.ready":
        requests.post(
            "https://search.internal/ingest",
            json={"document_id": event["document_id"], "library_id": event["library_id"]},
            timeout=10,
        )
```

Для приёма vendor-событий (обновление страницы Confluence → замена документа в IronRAG) — см. внешний middleware-проект; канонический вход — API IronRAG upload/replace/delete.
