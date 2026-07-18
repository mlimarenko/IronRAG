# IronRAG CLI

[Обзор](./README.md) | [IAM](./IAM.md) | [MCP](./MCP.md)

Инструмент командной строки для административных операций IronRAG. Подключается напрямую к PostgreSQL.

## Сборка

```bash
cargo build --release -p ironrag-backend --bin ironrag-cli
```

Бинарный файл также включен в Docker-образ по пути `/usr/local/bin/ironrag-cli`.

## Конфигурация

CLI использует те же переменные окружения, что и сервер. Обязательная переменная -- `DATABASE_URL` (или эквивалентная настройка из конфигурации приложения).

## Команды

### Версия CLI

```bash
ironrag-cli version
```

Выводит версию сборки CLI (совпадает с версией крейта `ironrag-backend`).

### Список пользователей

```bash
ironrag-cli list-users
```

Выводит таблицу всех пользователей с логином, отображаемым именем, статусом и датой создания.

### Создание пользователя

```bash
ironrag-cli create-user <LOGIN> <PASSWORD> [--name "Отображаемое имя"]
```

Создает нового пользователя с правами администратора (грант `iam_admin`). Пользователь автоматически добавляется в workspace по умолчанию, если он существует. Пароль должен содержать не менее 8 символов.

Параметры:
- `-n, --name` -- отображаемое имя (по умолчанию используется логин)

### Сброс пароля

```bash
ironrag-cli reset-password <LOGIN> <PASSWORD>
```

Обновляет пароль существующего пользователя и отзывает все активные сессии, требуя повторной аутентификации. Пароль должен содержать не менее 8 символов.

### Удаление пользователя

```bash
ironrag-cli delete-user <LOGIN>
```

Безвозвратно удаляет пользователя и все связанные записи (сессии, гранты, членство в workspace, principal).

### Создание API-токена

```bash
ironrag-cli create-token <LOGIN> [--label "my-token"] [--workspace "my-workspace"] [--permission <PERM>...] [--scope <SCOPE>]
```

Создает API-токен, привязанный к указанному пользователю. Токен в открытом виде отображается один раз и не может быть получен повторно. Токены имеют префикс `irt_`.

Параметры:
- `-l, --label` -- метка токена (по умолчанию `api-token`)
- `-w, --workspace` -- ограничить токен конкретным workspace (по slug или UUID)
- `-p, --permission` -- право доступа (можно указать несколько раз). Без указания по умолчанию `iam_admin`
- `--scope` -- явный скоуп гранта: `system`, `workspace:<slug>` или `library:<slug>`

Доступные права:
- `iam_admin` -- полное администрирование системы
- `workspace_admin`, `workspace_read` -- управление workspace
- `library_read`, `library_write` -- доступ к библиотекам и документам
- `document_read`, `document_write` -- доступ на уровне документа
- `query_run` -- выполнение запросов (ask)
- `ops_read`, `audit_read` -- операционные и аудит данные
- `credential_admin`, `binding_admin` -- управление интеграциями

Разрешение скоупа (когда `--scope` не указан):
- Системные права (`iam_admin`, `ops_read`, `audit_read`) → скоуп `system`
- Остальные права с `--workspace` → скоуп `workspace` на указанный workspace
- Остальные права без `--workspace` → скоуп `system` (доступ ко всем workspace)

Примеры:
```bash
# Полный админ-токен
ironrag-cli create-token admin

# Токен только на чтение для всех workspace
ironrag-cli create-token admin -p library_read -p query_run -l "reader"

# Токен на запись в конкретный workspace
ironrag-cli create-token admin -p library_read -p library_write -w default -l "writer"

# Токен для мониторинга
ironrag-cli create-token admin -p ops_read -p audit_read -l "monitoring"
```

### Список API-токенов

```bash
ironrag-cli list-tokens
```

Выводит все API-токены с principal ID, меткой, префиксом, статусом, датой выпуска и владельцем.

### Отзыв API-токена

```bash
ironrag-cli revoke-token <TOKEN_PRINCIPAL_ID>
```

Отзывает API-токен по UUID его principal. Устанавливает статус токена и principal в `revoked`.

### Список workspace

```bash
ironrag-cli list-workspaces
```

Выводит все workspace с ID, slug, отображаемым именем, состоянием жизненного цикла и датой создания.

### Создание workspace

```bash
ironrag-cli create-workspace <SLUG> [--name "Отображаемое имя"]
```

Создает новый workspace.

Параметры:
- `-n, --name` -- отображаемое имя (по умолчанию используется slug)

### Список библиотек

```bash
ironrag-cli list-libraries <WORKSPACE>
```

Выводит все библиотеки в workspace. Workspace можно указать по slug или UUID.

### Создание библиотеки

```bash
ironrag-cli create-library <WORKSPACE> <SLUG> [--name "Отображаемое имя"] [--description "Описание"]
```

Создает новую библиотеку в указанном workspace.

Параметры:
- `-n, --name` -- отображаемое имя (по умолчанию используется slug)
- `-d, --description` -- описание библиотеки

## Использование в Docker

```bash
docker exec <container> ironrag-cli list-users
docker exec <container> ironrag-cli create-user admin2 secretpass --name "Второй админ"
docker exec <container> ironrag-cli reset-password admin newpassword123
docker exec <container> ironrag-cli delete-user old-admin

docker exec <container> ironrag-cli create-token admin --label "ci-token" --workspace default
docker exec <container> ironrag-cli list-tokens
docker exec <container> ironrag-cli revoke-token <TOKEN_PRINCIPAL_ID>

docker exec <container> ironrag-cli list-workspaces
docker exec <container> ironrag-cli create-workspace staging --name "Staging"

docker exec <container> ironrag-cli list-libraries default
docker exec <container> ironrag-cli create-library default docs --name "Документация" --description "Публичная документация"
```

# IronRAG maintenance CLI

`ironrag-maintenance` — единая операторская поверхность для всего,
что поддерживает в порядке storage-слой IronRAG: посмотреть, что
лежит на диске, удалить то, что можно безопасно удалить,
восстановить то, что в сломанном состоянии, и проинспектировать
durable scheduler, который сам гоняет те же sweeper'ы в роли
worker'а.

Два инварианта, которые держим в голове на всех subcommand'ах:

* Каждая destructive-команда по умолчанию отказывается запускаться.
  Либо это явный opt-in (`--dry-run` выключен и нужен флаг типа
  `--yes`), либо команда берёт per-library advisory lock и
  отказывается работать, пока в библиотеке идёт ingest.
* Worker-контейнер крутит scheduler, который ходит по тем же
  sweeper'ам на rolling-каденции. Ручной CLI-вызов с ним не
  конфликтует: оба идут через одну и ту же lease-таблицу и
  блокируют друг друга чисто, если случайно подбираются на ту же
  строку.

## Когда что запускать?

Короткий decision-guide перед погружением в отдельные subcommand'ы.

| Симптом | Куда смотреть |
|---|---|
| «Диск кончается, что его съело?» | [`audit storage-summary`](#audit-storage-summary) |
| «Retrieval медленный, подозреваю плохие индексы» | [`audit index-bloat`](#audit-index-bloat) |
| «Документ загрузился, но в ответах не появляется» | [`audit null-head-docs`](#audit-null-head-docs), потом [`repair null-heads`](#repair-null-heads) |
| «Удалили библиотеки на той неделе, knowledge-plane rows вычищены?» | [`audit orphan-libraries`](#audit-orphan-libraries) |
| «Lifecycle webhook ушёл в dead-letter и блокирует catalog delete» | [`audit webhook-outbox`](#audit-webhook-outbox), затем requeue через [`repair webhook-outbox-dead-letter`](#repair-webhook-outbox-dead-letter) или явный discard через [`repair webhook-outbox-dead-letter-resolve`](#repair-webhook-outbox-dead-letter-resolve) |
| «Старые чанки копятся после замены ревизий» | [`gc stale-chunks`](#gc-stale-chunks) |
| «`runtime_graph_evidence` больше остальной БД вместе взятой» | [`gc stale-evidence`](#gc-stale-evidence) |
| «Подтверждённый orphan-след в knowledge plane, нужно вычистить» | [`gc orphan-libraries --yes`](#gc-orphan-libraries) |
| «Failed ingest наплодил кучу null-head документов» | [`repair null-heads-auto`](#repair-null-heads-auto) |
| «ingest_stage_event в архивах, запросы тормозят» | [`retention stage-events`](#retention-stage-events) |
| «Сменили embedding-модель библиотеки, вектора нужно перестроить» | [`rebuild vector-plane`](#rebuild-vector-plane) |
| «JSONL-чанки без temporal bounds» | [`migrate chunk-temporal-bounds`](#migrate-chunk-temporal-bounds) |
| «Хочу пере-embed всю библиотеку» | [`rebuild vector-plane`](#rebuild-vector-plane) |
| «Graph разъехался с документами» | [`rebuild runtime-graph`](#rebuild-runtime-graph) |
| «Что сейчас делает фоновый scheduler?» | [`lease summary`](#lease-summary) |
| «Sweeper ушёл в dead-letter, нужно вернуть после фикса root cause» | [`lease clear-failure`](#lease-clear-failure) |

## Сборка

```bash
cargo build --release -p ironrag-backend --bin ironrag-maintenance
```

Поставляется в Docker-образе по пути `/usr/local/bin/ironrag-maintenance`.

## Конфигурация

Использует те же переменные окружения, что и backend. Обязательная
database-настройка — `DATABASE_URL`.

## Замена устаревших maintenance-бинарей

`ironrag-maintenance` консолидирует per-task maintenance-бинари,
которые раньше шли вместе с `ironrag-backend`. Старые имена удалены —
вместо них вызывается новый subcommand.

| Удалённый бинарь | Subcommand-замена |
|---|---|
| `ironrag-gc-stale-chunks` | `ironrag-maintenance gc stale-chunks` |
| `ironrag-audit-orphan-data` | `ironrag-maintenance audit orphan-libraries` (read-only) + `ironrag-maintenance gc orphan-libraries --yes` (destructive) |
| `ironrag-promote-null-heads` | `ironrag-maintenance repair null-heads` |
| `ironrag-vector-rebuild` | `ironrag-maintenance rebuild vector-plane --source-library <uuid>` |
| `ironrag-backfill-chunk-temporal-bounds` | `ironrag-maintenance migrate chunk-temporal-bounds` |
| `rebuild_runtime_graph` | `ironrag-maintenance rebuild runtime-graph` |

---

## `audit` — read-only инспекция

Семейство audit отвечает на вопрос «что происходит» — никаких
изменений на диске. Безопасно запускать в любое время, в том числе
пока ingest и retrieval обрабатывают трафик.

### `audit storage-summary`

**Что показывает.** Топ Postgres-таблиц по размеру с
live/dead-tuple счётчиками и временем последнего autovacuum.

**Когда запускать.** Диск кончается. Запросы тормозят. Первый шаг
любого расследования «почему это так раздулось».

**Пример.**

```bash
ironrag-maintenance audit storage-summary --limit 20 --json
```

Если в выводе `runtime_graph_evidence` на 24 GB — это твой
единственный самый жирный кандидат на чистку, иди в
[`gc stale-evidence`](#gc-stale-evidence).

### `audit index-bloat`

**Что показывает.** Размер и количество сканов по каждому индексу
write-heavy таблиц. Колонка `idx_scan` говорит, как часто индекс
реально использовался — крупный индекс с нулём сканов это
кандидат на ревью под `DROP INDEX`.

**Когда запускать.** Подозреваешь bloat. Готовишься к окну
`REINDEX`. Нужны данные для PR про удалённый индекс.

**Пример.**

```bash
ironrag-maintenance audit index-bloat --min-size-mb 100 --json
```

Ограничить набор таблиц через `--tables=table_a,table_b`, когда
интересна одна подсистема.

### `audit null-head-docs`

**Что показывает.** Документы, у которых
`content_document_head` без readable и active ревизии. Retrieval их
игнорирует — обычно потому, что ingest упал до того, как продуктовая
ревизия была собрана. В выводе есть `recovery_attempts_count` и
`dead_letter_at`, чтобы видеть, как rate-limited recovery
потратила свой бюджет.

**Когда запускать.** Пользователи говорят «загрузил, в ответах
нет». После известного ingest-инцидента, до решения сколько
документов восстанавливать.

**Пример.**

```bash
ironrag-maintenance audit null-head-docs --library <uuid> --limit 100 --json
```

Если у документов в выводе `dead_letter_at IS NOT NULL` — recovery
уже исчерпала бюджет. См.
[`repair clear-recovery-dead-letter`](#repair-clear-recovery-dead-letter)
до повторной попытки.

### `audit orphan-libraries`

**Что показывает.** PostgreSQL knowledge-plane rows, чей `library_id`
не соответствует живой строке `catalog_library`. Такие строки
оставляли старые delete-пути и pre-cascade-fix код; в отчёте для
каждой orphan-библиотеки расписано, сколько строк осело в каких
knowledge tables.

**Когда запускать.** Регулярная гигиена, особенно после удалений
библиотек на старых деплоях. Всегда read-only — destructive чистка
это отдельная команда.

**Пример.**

```bash
ironrag-maintenance audit orphan-libraries --json
```

Непустой отчёт — каноничный pre-flight перед
[`gc orphan-libraries --yes`](#gc-orphan-libraries).

### `audit webhook-outbox`

**Что показывает.** Bounded и redacted inventory lifecycle outbox. По умолчанию используется
`--state dead-letter`, есть фильтр `--library`, на одной keyset-странице не больше 500 строк.
Payload, event ID, URL, credentials, headers, lease identity и raw errors исключены. Среди state
есть и `resolved`; вывод показывает только типизированные `last_error_code` /
`resolution_reason_code` и их несекретные timestamps.

**Пример.**

```bash
ironrag-maintenance audit webhook-outbox --state dead-letter --limit 100 --json
```

Если `has_more` равен true, продолжайте с обоими полями `next_cursor` через
`--before-created-at` и `--before-id`. Курсор опирается на неизменяемый порядок создания; поскольку
принадлежность state-фильтру меняется в реальном времени, после requeue во время paging начните новый
проход. Полный safety-контракт описан в
[webhook operations](./WEBHOOK.md#операции-с-lifecycle-outbox-dead-letter).

---

## `gc` — удаление мусора

Семейство gc удаляет контент, до которого канонические heads
больше не дотягиваются. Каждый gc subcommand берёт per-library
graph advisory lock и отказывается работать, пока в библиотеке есть
ingest job в состоянии `queued` / `leased` / `paused` — так
параллельный ingest не теряет данные из-за sweeper'а.

### `gc stale-chunks`

**Что делает.** Удаляет чанки из PostgreSQL knowledge-plane tables,
включая vector material, у которых revision больше не readable/active
head документа. По умолчанию — консервативно: документы, у которых
head null на обоих pointer'ах (failed ingest), пропускаются, чтобы
recoverable doc не был стёрт насовсем.

**Когда запускать.** Давление на PostgreSQL storage, особенно после
многих замен ревизий. Подозрение, что «остались старые чанки».

**Пример.**

```bash
# Безопасный preview: посчитать, не выполняя destructive-удаления.
ironrag-maintenance gc stale-chunks --dry-run --json

# Реальный запуск, одна библиотека.
ironrag-maintenance gc stale-chunks --library <uuid>

# Агрессивный режим: чистить также failed-ingest docs (только
# chunks/vectors; row документа остаётся). Использовать, когда
# доказано, что документ unrecoverable.
ironrag-maintenance gc stale-chunks --library <uuid> --include-null-head
```

### `gc stale-evidence`

**Что делает.** Удаляет строки `runtime_graph_evidence`, у которых
revision больше не readable/active head исходного документа, плюс
строки с `chunk_id` на чанк, который уже свипнул
`gc stale-chunks`. Оба lane пропускают строки, у которых документ
сейчас в активном ingest.

**Когда запускать.** Когда `audit storage-summary` показывает
`runtime_graph_evidence` как самую жирную таблицу. Обычно после
revision-тяжёлого периода или после `gc stale-chunks`, потому что
оба sweeper'а дополняют друг друга.

**Пример.**

```bash
ironrag-maintenance gc stale-evidence --library <uuid> --json
```

Пример вывода: `stale_revision_rows: 157645` означает, что 157k
строк в `runtime_graph_evidence` были привязаны к устаревшим
ревизиям. Спутник `phantom_chunk_rows` показывает строки,
привязанные к уже удалённым чанкам.

### `gc orphan-libraries`

**Что делает.** Destructive-спутник `audit orphan-libraries`.
Вычищает все PostgreSQL knowledge-plane rows, чей `library_id` не
соответствует живой строке `catalog_library`. Отказывается работать
без `--yes`.

**Когда запускать.** Только после того, как `audit orphan-libraries`
проверен и оператор принял его список.

**Пример.**

```bash
# Preview сначала
ironrag-maintenance audit orphan-libraries --json

# Потом purge
ironrag-maintenance gc orphan-libraries --yes --json
```

---

## `repair` — вернуть сломанный state в канон

Семейство repair — это про *восстановление*, не про удаление. Оно
пишет новые строки, чтобы привести объекты, разъехавшиеся с
канонической формой, обратно в state, который ingest pipeline даёт
на success.

### `repair null-heads`

**Что делает.** Для каждого документа с `readable_revision_id IS
NULL AND active_revision_id IS NULL`, у которого есть хотя бы одна
revision с persisted chunks, продвигает самую свежую chunk-bearing
revision в head. Использует тот же `promote_document_head`, что
ingest pipeline в success-flow, поэтому результат неотличим от
свежего успешного ingest. Идемпотентно.

**Когда запускать.** Single-shot восстановление за один проход
после известного инцидента. Когда нужно отработать каждый
eligible-документ сразу, без rate-limit.

**Пример.**

```bash
ironrag-maintenance repair null-heads --library <uuid> --json
```

### `repair null-heads-auto`

**Что делает.** То же recovery-действие, что `repair null-heads`,
но per-document результат теперь записывается на
`content_document_head`, чтобы flaky upstream не сжёг recovery-
бюджет на одном документе:

* Документ, тронутый за последний час, пропускается (cooldown).
* На success `recovery_attempts_count` сбрасывается,
  `last_recovery_attempt_at = now()`.
* На failure `recovery_attempts_count` инкрементируется, если
  новая ошибка совпадает с `last_recovery_error_code`; иначе
  счётчик сбрасывается в 1.
* Три подряд same-error failure'а ставят `dead_letter_at`,
  документ выпадает из будущих авто-проходов до тех пор, пока
  оператор не снимет метку.

**Когда запускать.** Перезапускаешь recovery на одну библиотеку
циклами или из cron. Везде, где flaky внешняя зависимость может
ронять один и тот же документ повторно — rate-limit превращает
это в управляемый backlog вместо tight retry storm.

**Пример.**

```bash
ironrag-maintenance repair null-heads-auto --library <uuid> --json
```

Пример вывода: `"promoted": 24, "failed": 0, "dead_lettered": 0,
"cooldown_skipped": 0` — за этот проход чисто восстановлено 24
документа; следующие проходы в течение часа эти 24 пропустят.

### `repair clear-recovery-dead-letter`

**Что делает.** Снимает `dead_letter_at` и recovery-счётчики с
`content_document_head` для одного документа. Следующий проход
`repair null-heads-auto` снова возьмёт его в работу.

**Когда запускать.** Только после того, как причина dead-letter
диагностирована и пофикшена.

**Пример.**

```bash
ironrag-maintenance repair clear-recovery-dead-letter --document <uuid>
```

### `repair webhook-outbox-dead-letter`

**Что делает.** Атомарно переводит один точный UUID outbox из `dead_letter` в `pending`, сбрасывает
attempt/lease/error state и делает строку доступной сейчас. HTTP не отправляет: доставку выполняет
штатный worker. Аудит показывает только стабильный типизированный `last_error_code`, но не сырые
ошибки транспорта и не payload. Отсутствующая строка или несовпадение state завершаются без изменений.

```bash
ironrag-maintenance repair webhook-outbox-dead-letter --outbox <uuid> --json
```

### `repair webhook-outbox-dead-letter-resolve`

**Что делает.** Навсегда переводит один точный UUID из `dead_letter` в отдельный terminal state
`resolved`, не выдавая это за доставку. Timestamp по часам PostgreSQL и bounded typed reason
атомарно сохраняются в outbox и durable redacted audit log. `dispatched` и все прочие состояния
защищены compare-and-set. Catalog delete считает `resolved` неблокирующим, а audit event переживает
catalog cascade.

Reason должен быть длиной 1–64 ASCII bytes в lowercase `snake_case`; free-form текст отклоняется.
Используйте команду только когда доставка намеренно больше не нужна. Если получатель всё ещё должен
получить событие, используйте requeue-команду выше. Без явного
`--acknowledge-not-delivered` команда ничего не меняет.

```bash
ironrag-maintenance repair webhook-outbox-dead-letter-resolve \
  --outbox <uuid> --reason-code receiver_retired --acknowledge-not-delivered --json
```

---

## `retention` — TTL-чистка history-таблиц

Семейство retention удаляет строки из INSERT-only history-таблиц,
которые перевалили за каноническую retention-windows. Каждая
прогонка батчуется (10 000 строк на DELETE, 100 мс пауза между
батчами) — конкурентные ingest-writer'ы остаются отзывчивыми.

### `retention stage-events`

**Что делает.** Удаляет строки `ingest_stage_event` старше
`--older-than-days`. Поддерживающий индекс
`idx_ingest_stage_event_recorded_at` поставлен миграцией 0017, и
predicate компилируется в index range scan вместо seq scan под
`AccessExclusiveLock`.

**Когда запускать.** Когда `audit storage-summary` показывает
`ingest_stage_event` как раздутую. Регулярно, когда запросы
ingest-истории `/v1/ingest/...` тормозят.

**Пример.**

```bash
# 90-дневный retention window
ironrag-maintenance retention stage-events --older-than-days 90 --json

# Будь осторожен с агрессивными значениями в dev/test
ironrag-maintenance retention stage-events --older-than-days 30 --json
```

---

## `migrate` — one-shot data migrations

Семейство migrate идемпотентно: повторный запуск после успеха —
no-op. Это НЕ recurring — они существуют для канонического пути
«конвертировать старую форму в новую» и удаляются из операторского
catalog'а, как только деплой полностью мигрирован.

### `migrate chunk-temporal-bounds`

**Что делает.** Backfill `occurred_at` / `occurred_until` для
чанков, у которых `normalized_text` содержит канонический JSONL
temporal header, но колонки всё ещё NULL. Cursor-пагинация по
chunk id, поэтому повторы после краха естественно продолжают
работу.

**Когда запускать.** Один раз на кластер, после апгрейда на
схему с temporal-колонками.

**Пример.**

```bash
# Preview без записи
ironrag-maintenance migrate chunk-temporal-bounds --dry-run --json

# Реальный запуск, одна библиотека
ironrag-maintenance migrate chunk-temporal-bounds --library <uuid> --json
```

## `rebuild` — тяжёлые operator-only passes

Семейство rebuild специально *никогда* не подключается к
recurring scheduler. Эти проходы тратят значительный provider-
бюджет или удерживают долго-живущие database resources; оператор
обязан запускать их явно с полным контекстом.

### `rebuild vector-plane`

**Что делает.** Согласует PostgreSQL pgvector material с активным
vector binding исходной библиотеки и пересобирает library
vector-материал, который должен перейти в соответствующую
`(library, dim)` relation.

**Когда запускать.** При смене embedding-размерности для библиотеки
или scope, где уже есть vector material. Исходная библиотека говорит
rebuilder'у, какой active binding и dimension использовать.

**Пример.**

```bash
ironrag-maintenance rebuild vector-plane --source-library <uuid>
```

### `rebuild runtime-graph`

**Что делает.** Перезапускает канонический runtime-graph
projection для одной библиотеки или для всех. Batch-mode терпит
per-library `StateConflict` ошибки и в конце даёт non-zero exit,
чтобы operator-скрипты могли распознать partial completion.

**Когда запускать.** Граф видимо разъехался с документами
(например, удаления документов применились, но graph edges всё
ещё на них ссылаются). После schema migration, тронувшей graph
projection.

**Пример.**

```bash
# Одна библиотека
ironrag-maintenance rebuild runtime-graph --library <uuid>

# Все библиотеки (batch mode)
ironrag-maintenance rebuild runtime-graph
```

---

## `lease` — внутренности scheduler'а

Фоновый scheduler отслеживает каждую (class, scope) maintenance-
единицу в строке `maintenance_job_run`. Subcommand'ы lease
инспектируют эту таблицу и управляют восстановлением после ошибок.

### `lease show`

**Что показывает.** Текущую lease-строку для каждой (class,
scope), которую scheduler отслеживает, в том числе кто держит её
сейчас, какой у неё `next_due_at` и последняя ошибка, если есть.

**Когда запускать.** Расследуешь, почему scheduler не подбирает
класс. После dead-letter alert'а.

**Пример.**

```bash
ironrag-maintenance lease show --class gc.stale-chunks --json
ironrag-maintenance lease show --state dead_letter --json
```

### `lease summary`

**Что показывает.** Сводку по классу: pending / leased /
completed / failed / dead-letter. Компактный обзор state'а
scheduler'а.

**Когда запускать.** Quick-check здоровья, подходит для cron-style
мониторинга.

**Пример.**

```bash
ironrag-maintenance lease summary --json
```

### `lease clear-failure`

**Что делает.** Сбрасывает dead-letter lease-строку обратно в
`pending`, чтобы scheduler подобрал её на следующем тике.
Использовать после фикса root cause.

**Пример.**

```bash
# Per-library class
ironrag-maintenance lease clear-failure --class gc.stale-chunks --library <uuid>

# Instance-scope class (без --library)
ironrag-maintenance lease clear-failure --class retention.stage-events
```

### `lease reap-stale`

**Что делает.** Подбирает leased-строки, у которых heartbeat
старше порога. Scheduler делает это на каждом тике; команда —
ручной рычаг, если оператор подозревает, что что-то застряло.

**Пример.**

```bash
ironrag-maintenance lease reap-stale --stale-after-secs 300
```

---

## Использование в Docker

```bash
docker exec <container> ironrag-maintenance audit storage-summary --json
docker exec <container> ironrag-maintenance gc stale-chunks --dry-run
docker exec <container> ironrag-maintenance lease summary --json
```
