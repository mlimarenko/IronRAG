<div align="center">

# Webhooks IronRAG

### Исходящие webhooks: рассылка событий revision.ready и document.deleted подписчикам

[Обзор](./README.md) | [Webhooks (EN)](../en/WEBHOOK.md) | [Шифрование credential](./CREDENTIAL-ENCRYPTION.md) | [MCP](./MCP.md) | [IAM](./IAM.md) | [CLI](./CLI.md)

</div>

## Обзор

IronRAG отправляет исходящие webhooks, уведомляя внешние системы об изменениях состояния. Приём входящих событий от vendor-систем (Confluence, MediaWiki, Notion и др.) — ответственность внешней middleware-прослойки, которая напрямую вызывает HTTP API IronRAG (upload / replace / delete).

**Outbound** — `webhook_subscription` регистрирует HTTPS-эндпоинты, получающие HMAC-подписанные события об изменениях состояния IronRAG (`revision.ready`, `document.deleted`). Доставка durable: каждая отправка — `ingest_job` с `job_kind=webhook_delivery`, существующий пул воркеров обрабатывает lease/heartbeat/retry. На неудачу — экспоненциальный backoff до 8 попыток (максимальная задержка 128 минут), затем `abandoned`.

## Модель подписки

Строки `webhook_subscription` описывают HTTP-приёмники событий IronRAG.

```
POST /v1/webhooks/subscriptions
Authorization: Bearer <api-token с workspace_admin>
Content-Type: application/json

{
  "workspaceId": "<uuid>",
  "libraryId": "<uuid или null для workspace-wide>",
  "displayName": "Downstream индекс",
  "targetUrl": "https://hooks.example.com/ironrag-events",
  "secret": "<random 32+ байта hex>",
  "eventTypes": ["revision.ready", "document.deleted"],
  "customHeaders": {
    "Authorization": "Bearer <receiver-token>"
  }
}
```

| Поле | Заметки |
|------|---------|
| `workspaceId` | Scope. Доставляются только события из этого workspace |
| `libraryId` | Опционально. Если указан — только события из этой library; null = все libraries в workspace |
| `eventTypes` | Непустой массив event names |
| `secret` | HMAC-SHA256 ключ для исходящих подписей |
| `customHeaders` | До 32 валидированных строковых headers, зашифрованных at rest. Служебные framing/signature/trace headers зарезервированы |
| `active` | По умолчанию `true`; `false` → подписка приостановлена |

В одном workspace может быть не более 100 активных подписок. Workspace-scoped
DB trigger сериализует create/reactivation на границе транзакции, поэтому
конкурентные запросы и старые API pod-ы в rolling deploy используют один
fanout-лимит; неактивные подписки его не занимают. Legacy-превышения видны в
redacted audit view. Общее число активных и неактивных строк ограничено 1 000
на workspace. IronRAG не удаляет неактивные подписки автоматически по возрасту:
связанные delivery-строки являются операционным audit evidence. Перед созданием
новых подписок удаляйте ненужные неактивные явно.

CRUD-эндпоинты:

- `GET    /v1/webhooks/subscriptions?workspaceId=`
- `GET    /v1/webhooks/subscriptions/{id}`
- `POST   /v1/webhooks/subscriptions`
- `PATCH  /v1/webhooks/subscriptions/{id}`
- `DELETE /v1/webhooks/subscriptions/{id}`
- `GET    /v1/webhooks/subscriptions/{id}/attempts`

Оба list endpoint используют bounded keyset pagination по `(createdAt, id)`,
сохраняя прежний JSON array в response. Для подписок default page = 100, для
delivery attempts = 200; `limit` в обоих случаях ограничен сверху 200. Для
следующей страницы передайте `createdAt` и `id` последнего элемента как пару
`afterCreatedAt` + `afterId`; один компонент без второго возвращает `400`.
Подписки остаются в порядке от старых к новым, attempts — от новых к старым.
Management projection не загружает signing secrets, custom-header ciphertext,
signed payload, response body, queue job ID и delivery lease token.

Global UUID подписки фильтруется в SQL по разрешённому workspace scope
вызывающего. Отсутствующая и чужая подписка одинаково возвращают `404` для GET,
PATCH, DELETE и списка attempts, поэтому UUID нельзя использовать для tenant
enumeration.

`DELETE` возвращает `204` только когда не осталось владельцев claimed delivery.
Если POST уже захвачен worker-ом, первый вызов атомарно ставит tombstone и
возвращает `202 Accepted`; повторяйте тот же DELETE до `204`. Tombstoned
подписку нельзя реактивировать через PATCH. Один только возраст lease не
доказывает, что внешний HTTP side effect остановлен.

Доставка имеет семантику at-least-once: remote endpoint мог принять POST до
того, как worker потерял DB lease или успел сохранить результат. Получатель
должен идемпотентно дедуплицировать запросы по `X-IronRAG-Event-Id` (то же
значение находится в `event_id` body). Lease-token защищает запись результата
в БД, но не превращает внешний HTTP side effect в exactly-once. Для crashed
draining owner нужен явный operator command
`ironrag-maintenance repair webhook-delivery-abandon --subscription <uuid> --acknowledge-duplicate-delivery-risk`; обычный DELETE не считает возраст lease подтверждением остановки.

## События

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
  "revision_id": "<uuid>"
}
```

### `document.deleted`

Записывается атомарно вместе с сохранённым переходом документа в soft-delete и
становится доступным для доставки только после коммита этой транзакции. Cleanup
проекций выполняется независимо и не может удалить durable lifecycle event.

```json
{
  "event_type": "document.deleted",
  "event_id": "document.deleted:<document_uuid>:<deleted_at_unix_microseconds>",
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

Плюс любые `customHeaders` подписки.

Raw signed body — плоский JSON-объект из каталога событий выше. Перед постановкой
в очередь IronRAG перезаписывает `event_type`, `event_id`, `occurred_at`,
`workspace_id` и `library_id` каноническими сохранёнными метаданными, поэтому
producer payload не может подменить поля маршрутизации или дедупликации.

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
| HTTP 5xx, 429, network/timeout | `attempt_number++`; попытки 1–7 планируются через 2, 4, 8, 16, 32, 64 и 128 минут. Попытка 8 → `abandoned` |
| HTTP 4xx (прочие) | `failed`, без retry |

Replay-защита: получатели ДОЛЖНЫ отклонять delivery с `t=` за пределами ±5 минут от своих часов.

### Обновление с delivery fencing

Expansion-миграция добавляет token-fenced completion, но не переписывает и не
reclaim-ит legacy tokenless in-flight owner: старый процесс во время rolling
overlap всё ещё может продолжить работу и сохранить результат. Обновлённые
workers reclaim-ят только lease, созданные уже с current-protocol token.
Постоянный DB trigger удаляет response body и динамические legacy error strings,
а deferred commit trigger создаёт корректно scoped queue job, если старый
двухтранзакционный publisher закоммитил unlinked pending attempt.

Crashed tokenless owner остаётся fail-closed и требует явного abandon command с
подтверждением duplicate-delivery риска, описанного выше. После drain старых
writers оператор проверяет redacted lease/tenant/contract audit views; только
следующая contract migration может валидировать restrictive lease-shape и
subscription field checks.

## Гарантия безопасного удаления catalog

Удаление library и workspace работает fail-closed относительно durable webhook-работы. Транзакция
удаления library блокирует её строку `catalog_library`; транзакция удаления workspace блокирует
строку `catalog_workspace` и все строки дочерних libraries. Затем удаление отклоняется, пока во
всём затронутом scope выполняется хотя бы одно условие:

- в scope есть строка `webhook_lifecycle_outbox` в состоянии `pending`, `dispatching` или
  `dead_letter`; явные terminal states `dispatched` и `resolved` удаление не блокируют; или
- в scope есть ingest job типа `webhook_delivery` в состоянии `queued`, `leased`, `paused` или
  `failed`; или
- delivery attempt остаётся `pending`/`delivering` либо retryable `failed`, но для
  него нет соответствующего активного queue job (включая crash-window старого
  publisher между create и link).

Catalog-объект удаляется той же транзакцией только после полного drain этих строк. Блокировки
родительских строк также отсекают конкурентные FK-backed вставки library/outbox/job: новая строка
не может попасть между проверкой и cascade-delete и тем самым бесшумно потерять событие.
Заблокированное исполнение маппится в типизированный catalog conflict (`409` на синхронных
поверхностях). REST-маршрут удаления catalog асинхронный и сначала отвечает `202`; если blocker
появляется до запуска воркера,
async operation завершается как failed, а удаление нужно повторить после drain outbox/job. В
частности, событие `dead_letter` требует явного решения оператора и никогда не отбрасывается просто
через удаление library или workspace. Строка `resolved` не блокирует удаление, а её redacted global
audit event переживает последующий catalog cascade.

В текущем enum `ingest_queue_state` не блокируют удаление только webhook-delivery jobs в состояниях
`completed` и `canceled`. `abandoned` — состояние delivery attempt, а не очереди ingest job. Любое
будущее или неизвестное состояние очереди по умолчанию считается незавершённым и блокирует
удаление.

## Операции с lifecycle outbox dead-letter

Вместо прямых запросов к webhook-таблицам с секретными данными используйте bounded audit:

```bash
ironrag-maintenance audit webhook-outbox --state dead-letter --limit 100
ironrag-maintenance audit webhook-outbox --state dead-letter --library <library-uuid> --json
```

Допустимые state-фильтры: `pending`, `dispatching`, `dispatched`, `dead-letter` и `resolved`; по умолчанию
используется `dead-letter`, а команда отклоняет limit вне `1..=500`. В вывод попадают только UUID
outbox, тип события, workspace/library scope, state/счётчик попыток, типизированные failure/resolution
reason codes и timestamps. Payload, event ID,
URL получателя, signing secret, custom headers, lease identity/token и raw-текст ошибки не выводятся.

Если есть следующая страница, JSON содержит `has_more=true` и объект `next_cursor`, а human output
печатает точные continuation flags. Продолжайте с обоими компонентами keyset и теми же фильтрами:

```bash
ironrag-maintenance audit webhook-outbox --state dead-letter \
  --before-created-at <next_cursor.created_at> --before-id <next_cursor.id>
```

Так stable keyset pagination позволяет просмотреть все строки без unbounded read; только один
компонент cursor команда не принимает.

После диагностики и исправления receiver/configuration верните в очередь один точный UUID:

```bash
ironrag-maintenance repair webhook-outbox-dead-letter --outbox <outbox-uuid> --json
```

Это атомарный compare-and-set только из `dead_letter` в `pending`: он сбрасывает attempts, lease
поля и raw error, затем выставляет немедленную доступность строки. Команда не выполняет HTTP-запрос;
доставку позже делает штатный worker loop. Отсутствующая строка или строка, state которой уже
изменился, остаётся нетронутой, а команда завершается с ненулевым кодом. В аудите виден только
стабильный типизированный `last_error_code`; payload и сырые ошибки транспорта maintenance-процесс
вообще не загружает.

Если доставка навсегда не нужна, явно resolve'ните точный dead-letter, не выдавая это за успешную
доставку получателю:

```bash
ironrag-maintenance repair webhook-outbox-dead-letter-resolve \
  --outbox <outbox-uuid> --reason-code receiver_retired \
  --acknowledge-not-delivered --json
```

Это отдельный атомарный compare-and-set из `dead_letter` в `resolved`. Reason обязан быть bounded
machine code в lowercase `snake_case` (1–64 ASCII bytes); free-form текст отклоняется, чтобы оператор
случайно не сохранил URL, credential или response body. Строка получает
`resolution_reason_code` и рассчитанный по часам PostgreSQL `resolved_at`, а тот же SQL statement
добавляет redacted audit event `webhook.lifecycle_outbox.dead_letter_resolved`. Команда никогда не
меняет `dispatched` и не утверждает, что доставка состоялась. Если доставку всё ещё надо выполнить,
используйте requeue-команду выше. Явный флаг `--acknowledge-not-delivered` обязателен.

## Детект изменений только-картинок

Когда PDF, DOCX или PPTX заменяет встроенную картинку без изменения OCR-текста, существующий `text_checksum` не изменился бы и стандартный chunk-reuse plan пропустил бы re-embedding. Чтобы это исправить, IronRAG считает revision-level `image_checksum` (sort всех байтов извлечённых картинок, затем SHA-256). Когда `parent.image_checksum != new.image_checksum`, chunk-reuse plan байпасится и embeddings + graph extraction пересчитываются полностью для этой ревизии. Семантика `text_checksum` сохранена (только текст).

## Операционные заметки

- **Секреты** и сериализованные значения **custom headers** хранятся отдельными аутентифицированными row-bound AEAD-конвертами `ironrag:enc:v3` в `webhook_subscription.secret` и `webhook_subscription.custom_headers_json`. Расшифрование происходит только внутри delivery; purpose, UUID подписки и key ID аутентифицируются. Для существующей установки сначала нужно развернуть dual-reader с выключенной encrypted-записью, а затем включить запись отдельным rollout. Ротация master key требует трёх фаз с перекрывающимся keyring. Используйте полный [runbook шифрования credential](./CREDENTIAL-ENCRYPTION.md); неизвестный key ID или ошибка аутентификации всегда обрабатывается fail-closed.
- **Job queue** общая с ingest-пайплайном. `job_kind=webhook_delivery` конкурирует с `content_mutation`, `web_discovery`, `web_materialize_page` за worker-leases. Тяжёлая outbound нагрузка может тормозить ingest; тюнить `IRONRAG_INGESTION_WORKER_POOL_SIZE`.
- **Lease lifecycle relay**: relay берёт по одной outbox-строке и каждые 60 секунд продлевает пятиминутный lease через token-fenced CAS по часам PostgreSQL. Потеря lease или ошибка renewal отменяет оставшийся recipient fanout; детерминированный per-recipient dedupe безопасно сводит повтор нового owner.
- **Наблюдаемость**: каждая outbound-попытка записана в `webhook_delivery_attempt` и запрашивается SQL для forensics. Воркеры эмитят `tracing` spans на стадии `webhook_delivery`. OTLP-метрики: `ironrag.webhook.lifecycle_outbox.event_age_seconds`, `drain_duration_seconds`, `lease_conflicts`, `lease_renewals` и `outcomes`.

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

Для приёма vendor-событий (обновление страницы Confluence → замена документа в IronRAG) — см. внешний middleware-проект; вход — API IronRAG upload/replace/delete.
