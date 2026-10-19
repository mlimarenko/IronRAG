<div align="center">

# Вебхуки IronRAG

### Исходящие вебхуки: рассылка событий `revision.ready` и `document.deleted`

[Обзор](./README.md) | [Webhooks (EN)](../en/WEBHOOK.md) | [Шифрование credential](./CREDENTIAL-ENCRYPTION.md) | [MCP](./MCP.md) | [IAM](./IAM.md) | [CLI](./CLI.md)

</div>

## Обзор

IronRAG отправляет исходящие вебхуки внешним системам при изменении состояния.
Входящие события поставщиков (Confluence, MediaWiki, Notion и других) принимает
внешний промежуточный сервис, который вызывает HTTP API IronRAG для загрузки,
замены и удаления документов.

**Исходящая подписка** — строка `webhook_subscription` с HTTP(S)-адресом,
получающим HMAC-подписанные события IronRAG (`revision.ready`,
`document.deleted`). Каждая отправка создаёт `ingest_job` с
`job_kind=webhook_delivery`; существующий пул воркеров обрабатывает аренду,
продление и повторы. Неудачная доставка повторяется с экспоненциальной задержкой
до восьми попыток (максимум 128 минут), затем получает состояние `abandoned`.

## Модель подписки

Строки `webhook_subscription` описывают HTTP-приёмники событий IronRAG.

```
POST /v1/webhooks/workspaces/<workspace_uuid>/subscriptions
Authorization: Bearer <api-token с workspace_admin>
Content-Type: application/json

{
  "libraryId": "<uuid или null для workspace-wide>",
  "displayName": "Downstream индекс",
  "targetUrl": "https://hooks.example.com/ironrag-events",
  "secret": "<не менее 32 байтов случайных данных>",
  "eventTypes": ["revision.ready", "document.deleted"],
  "customHeaders": {
    "Authorization": "Bearer <receiver-token>"
  }
}
```

| Поле | Заметки |
|------|---------|
| `workspace_uuid` в пути | Область: доставляются только события этого рабочего пространства |
| `libraryId` | Необязательно. Если указан — только события этой библиотеки; `null` — всех библиотек рабочего пространства |
| `eventTypes` | Непустой массив из 1–8 различных поддерживаемых имён событий |
| `secret` | HMAC-SHA256 ключ для исходящих подписей |
| `customHeaders` | До 32 проверенных строковых заголовков, зашифрованных при хранении. Служебные заголовки фрейминга, подписи и трассировки зарезервированы |
| `active` | По умолчанию `true`; `false` → подписка приостановлена |

В рабочем пространстве может быть не более 100 активных подписок. Триггер базы
данных сериализует создание и повторную активацию в транзакции, поэтому
конкурентные запросы используют один предел рассылки; неактивные подписки его
не занимают. Всего может быть до 1 000 активных и неактивных строк на рабочее
пространство. IronRAG не удаляет неактивные подписки по возрасту: связанные
строки доставки нужны для операционного аудита. Перед созданием новых подписок
явно удалите ненужные неактивные.

CRUD-эндпоинты:

- `GET    /v1/webhooks/workspaces/{workspaceId}/subscriptions`
- `GET    /v1/webhooks/subscriptions/{subscriptionId}`
- `POST   /v1/webhooks/workspaces/{workspaceId}/subscriptions`
- `PATCH  /v1/webhooks/subscriptions/{subscriptionId}`
- `DELETE /v1/webhooks/subscriptions/{subscriptionId}`
- `GET    /v1/webhooks/subscriptions/{subscriptionId}/attempts`

Оба списка используют ограниченную постраничную выборку по `(createdAt, id)` и
возвращают объект с `items` и, если есть следующая страница, непрозрачным
`nextCursor`. Для подписок размер страницы по умолчанию — 100, для попыток
доставки — 200; `limit` в обоих случаях ограничен 200. Для следующей страницы
передайте `cursor=<nextCursor>` без изменений. Подписки идут от старых к новым,
попытки — от новых к старым. Управляющая проекция не загружает секреты подписи,
шифротекст пользовательских заголовков, подписанные полезные нагрузки, тела
ответов, идентификаторы заданий очереди и токены аренды доставки.

Глобальный UUID подписки SQL фильтрует по разрешённой вызывающему области
рабочего пространства. Отсутствующая и чужая подписка одинаково возвращают
`404` для GET, PATCH, DELETE и списка попыток, поэтому UUID нельзя использовать
для перечисления арендаторов.

`DELETE` возвращает `204`, только когда не осталось владельца выполняющейся
доставки. Если worker уже владеет POST, первый вызов атомарно помечает подписку
для удаления и возвращает `202 Accepted`; повторяйте DELETE до `204`. Такую
подписку нельзя повторно активировать через PATCH. Возраст аренды сам по себе не
доказывает остановку внешнего HTTP-действия.

Доставка выполняется как минимум один раз: удалённый адрес мог принять POST до
того, как worker потерял аренду базы или сохранил результат. Получатель должен
идемпотентно дедуплицировать запросы по `X-IronRAG-Event-Id` (это же значение
находится в `event_id` тела). Токен аренды защищает запись результата в БД, но
не делает внешнее HTTP-действие ровно однократным. Для аварийно завершившегося
владельца ожидающей удаления доставки нужна явная команда:
`ironrag-maintenance repair webhook-delivery-abandon --subscription <uuid> --acknowledge-duplicate-delivery-risk`.
Обычный DELETE не считает возраст аренды подтверждением остановки.

## События

### `revision.ready`

Событие возникает, когда ingest-пайплайн завершил ревизию и сделал её доступной
для чтения. Оно отправляется после успешной загрузки, замены, добавления или
редактирования.

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

Событие записывается атомарно с сохранённым переходом документа в мягкое
удаление и становится доступным для доставки только после фиксации транзакции.
Очистка проекций выполняется независимо и не удаляет долговечное событие
жизненного цикла.

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

Также отправляются `customHeaders` подписки.

Подписываемое тело — плоский JSON-объект из каталога событий. Перед постановкой
в очередь IronRAG перезаписывает `event_type`, `event_id`, `occurred_at`,
`workspace_id` и `library_id` сохранёнными метаданными, поэтому payload
поставщика не может подменить поля маршрутизации или дедупликации.

Вход HMAC: `<ts_unix_seconds>.<raw байты тела>`; точка `.` буквальная. Ключ HMAC
— `subscription.secret`.

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

**Не пересериализуйте тело** между получением и проверкой: байты должны
совпадать полностью.

## Повторные попытки

| Результат | Поведение |
|-----------|-----------|
| HTTP 2xx | `delivered`, проставляется `delivered_at` |
| HTTP 5xx, 429, ошибка сети или тайм-аут | `attempt_number++`; попытки 1–7 назначаются через 2, 4, 8, 16, 32, 64 и 128 минут. Попытка 8 → `abandoned` |
| Другой HTTP 4xx | `failed`, без повтора |

Защита от повтора: получатель ДОЛЖЕН отклонять доставку с `t=` за пределами
±5 минут от собственных часов.

## Безопасное удаление каталога

Удаление библиотеки и рабочего пространства работает по умолчанию с отказом,
пока есть долговечная работа вебхуков. Транзакция удаления библиотеки блокирует
её строку `catalog_library`; транзакция удаления рабочего пространства блокирует
строку `catalog_workspace` и строки всех дочерних библиотек. Затем удаление
отклоняется, пока в затронутой области выполняется хотя бы одно условие:

- есть строка `webhook_lifecycle_outbox` в состоянии `pending`, `dispatching` или
  `dead_letter`; конечные состояния `dispatched` и `resolved` удаление не
  блокируют; или
- есть ingest job типа `webhook_delivery` в состоянии `queued`, `leased`,
  `paused` или `failed`; или
- попытка доставки остаётся `pending`/`delivering` либо повторяемой `failed`,
  но без соответствующего активного задания очереди.

Объект каталога удаляется той же транзакцией только после завершения этой
работы. Блокировки родительских строк также не допускают конкурентные
FK-поддерживаемые вставки библиотеки, outbox или job между проверкой и каскадным
удалением, чтобы событие не потерялось бесшумно. Блокировка преобразуется в
типизированный конфликт каталога (`409` на синхронных поверхностях). REST-маршрут
удаления каталога асинхронный и сначала возвращает `202`; если блокер появится
до запуска worker, асинхронная операция завершится с `failed`, а удаление нужно
повторить после завершения outbox/job. Событие `dead_letter` требует явного
решения оператора и не отбрасывается простым удалением библиотеки или рабочего
пространства. Строка `resolved` не блокирует удаление, а её обезличенное
глобальное событие аудита переживает каскадное удаление каталога.

В текущем enum `ingest_queue_state` не блокируют удаление только задания
webhook-delivery в состояниях `completed` и `canceled`. `abandoned` — состояние
попытки доставки, а не очереди ingest job. Любое будущее или неизвестное
состояние очереди по умолчанию считается незавершённым и блокирует удаление.

## Операции с dead-letter lifecycle outbox

Вместо прямых запросов к таблицам вебхуков с секретными данными используйте
ограниченную проверку:

```bash
ironrag-maintenance audit webhook-outbox --state dead-letter --limit 100
ironrag-maintenance audit webhook-outbox --state dead-letter --library <library-uuid> --json
```

Допустимые фильтры состояния: `pending`, `dispatching`, `dispatched`,
`dead-letter` и `resolved`; по умолчанию используется `dead-letter`, а команда
отклоняет limit вне `1..=500`. В вывод попадают только UUID outbox, тип события,
область рабочего пространства и библиотеки, состояние и число попыток,
типизированные коды ошибки и причины разрешения, а также временные метки. Не
выводятся payload, event ID, URL получателя, секрет подписи, пользовательские
заголовки, идентификатор или токен аренды и исходный текст ошибки.

Если есть следующая страница, JSON содержит `has_more=true` и объект
`next_cursor`, а обычный вывод печатает точные флаги продолжения. Продолжайте с
обоими компонентами keyset и теми же фильтрами:

```bash
ironrag-maintenance audit webhook-outbox --state dead-letter \
  --before-created-at <next_cursor.created_at> --before-id <next_cursor.id>
```

Такая стабильная постраничная выборка позволяет просмотреть все строки без
неограниченного чтения; один компонент cursor команда не принимает.

После диагностики и исправления получателя или конфигурации верните в очередь
один точный UUID:

```bash
ironrag-maintenance repair webhook-outbox-dead-letter --outbox <outbox-uuid> --json
```

Это атомарный compare-and-set только из `dead_letter` в `pending`: он сбрасывает
попытки, поля аренды и исходную ошибку и сразу делает строку доступной. Команда
не выполняет HTTP-запрос; доставку позже запускает штатный worker. Отсутствующая
строка или строка с изменившимся состоянием остаётся нетронутой, а команда
завершается с ненулевым кодом. В аудите виден только стабильный типизированный
`last_error_code`; процесс обслуживания не загружает payload и исходные ошибки
транспорта.

Если доставка больше не нужна, явно разрешите точный dead-letter, не выдавая это
за успешную доставку получателю:

```bash
ironrag-maintenance repair webhook-outbox-dead-letter-resolve \
  --outbox <outbox-uuid> --reason-code receiver_retired \
  --acknowledge-not-delivered --json
```

Это отдельный атомарный compare-and-set из `dead_letter` в `resolved`. Причина
должна быть ограниченным машинным кодом в нижнем `snake_case` (1–64 ASCII-байта);
свободный текст отклоняется, чтобы оператор не сохранил URL, учётные данные или
тело ответа. Строка получает `resolution_reason_code` и рассчитанный по часам
PostgreSQL `resolved_at`, а то же SQL-выражение добавляет обезличенное событие
аудита `webhook.lifecycle_outbox.dead_letter_resolved`. Команда не меняет
`dispatched` и не утверждает, что доставка состоялась. Если доставка всё ещё
нужна, используйте команду возврата в очередь выше. Явный флаг
`--acknowledge-not-delivered` обязателен.

## Обнаружение изменений только изображения

Если PDF, DOCX или PPTX заменяет встроенное изображение без изменения текста,
доступного OCR, `text_checksum` не меняется и обычный план повторного
использования chunk пропускает повторное embedding. Поэтому IronRAG вычисляет
`image_checksum` для ревизии: сортирует байты извлечённых изображений и считает
SHA-256. При `parent.image_checksum != new.image_checksum` план повторного
использования chunk отключается, а embedding и извлечение графа для ревизии
выполняются заново. Семантика `text_checksum` остаётся текстовой.

## Операционные заметки

- **Секреты** и сериализованные значения **custom headers** хранятся отдельными
  аутентифицированными AEAD-конвертами `ironrag:enc:v3`, привязанными к строке,
  в `webhook_subscription.secret` и `webhook_subscription.custom_headers_json`.
  Расшифрование происходит только во время доставки; конверт аутентифицирует
  назначение, UUID подписки и key ID. Шифрование учётных данных обязательно, а
  master key требуется при старте. Ротация master key
  требует трёх фаз с перекрывающимся keyring. Используйте полный [runbook
  шифрования credential](./CREDENTIAL-ENCRYPTION.md); неизвестный key ID или
  ошибка аутентификации всегда приводят к отказу.
- **Очередь заданий** общая с ingest-пайплайном. `job_kind=webhook_delivery`
  конкурирует с `content_mutation`, `web_discovery` и `web_materialize_page` за
  аренду worker. Большая исходящая нагрузка может замедлить ingest; настройте
  `IRONRAG_INGESTION_WORKER_POOL_SIZE`.
- **Аренда lifecycle relay**: relay берёт по одной строке outbox и каждые 60
  секунд продлевает пятиминутную аренду token-fenced CAS по часам PostgreSQL.
  Потеря аренды или ошибка продления отменяет оставшуюся рассылку получателям;
  детерминированная дедупликация для каждого получателя безопасно сводит повтор
  нового владельца.
- **Наблюдаемость**: каждая исходящая попытка записывается в
  `webhook_delivery_attempt` и доступна через SQL для расследования. Workers
  создают spans `tracing` на этапе `webhook_delivery`. Метрики OTLP:
  `ironrag.webhook.lifecycle_outbox.event_age_seconds`, `drain_duration_seconds`,
  `lease_conflicts`, `lease_renewals` и `outcomes`.

## Пример исходящей доставки

IronRAG отправляет `revision.ready` после завершения ingest документа. Подписчик
передаёт событие в поисковый индекс:

```python
import hmac, hashlib, time, json, requests

def verify_and_forward(secret: bytes, header: str, body: bytes):
    # Проверить подпись IronRAG
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

Для приёма событий поставщика (обновление страницы Confluence → замена документа
в IronRAG) используйте внешний промежуточный сервис; его точка входа — API
IronRAG для загрузки, замены и удаления документов.
