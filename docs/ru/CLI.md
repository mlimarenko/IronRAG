# IronRAG CLI

[Обзор](./README.md) | [IAM](./IAM.md) | [MCP](./MCP.md)

Инструмент командной строки для административных операций IronRAG. Подключается напрямую к PostgreSQL.

## Сборка

```bash
cargo build --release -p ironrag-backend --bin ironrag-cli
```

Бинарный файл также включен в Docker-образ по пути `/usr/local/bin/ironrag-cli`.

## Конфигурация

CLI использует те же переменные окружения, что и сервер. Обязательная
переменная — `DATABASE_URL`.

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

### Администрирование IAM

Изменения пользователей, паролей, прав и жизненного цикла токенов намеренно
недоступны через CLI с прямым доступом к БД. Используйте аутентифицированные
конечные точки `/v1/iam`: они проверяют роль администратора, область действия
ресурса и записывают аудит. API-токены используют формат `inrg_`.

### Список workspace

```bash
ironrag-cli list-workspaces
```

Выводит все рабочие пространства с ID, slug, отображаемым именем, состоянием
жизненного цикла и датой создания.

### Создание workspace

```bash
ironrag-cli create-workspace <SLUG> [--name "Отображаемое имя"]
```

Создаёт новое рабочее пространство.

Параметры:
- `-n, --name` — отображаемое имя; по умолчанию используется slug

### Список библиотек

```bash
ironrag-cli list-libraries <WORKSPACE>
```

Выводит все библиотеки рабочего пространства. Рабочее пространство можно
указать по slug или UUID.

### Создание библиотеки

```bash
ironrag-cli create-library <WORKSPACE> <SLUG> [--name "Отображаемое имя"] [--description "Описание"]
```

Создаёт новую библиотеку в указанном рабочем пространстве.

Параметры:
- `-n, --name` — отображаемое имя; по умолчанию используется slug
- `-d, --description` — описание библиотеки

## Использование в Docker

Для операций IAM используйте аутентифицированные конечные точки `/v1/iam`.
CLI с прямым доступом к БД предназначен только для не-IAM команд.

# IronRAG maintenance CLI

`ironrag-maintenance` — единый инструмент оператора для обслуживания хранилища
IronRAG: просмотра занятого места, безопасного удаления производных данных,
восстановления повреждённого состояния и проверки постоянного планировщика,
который запускает те же задачи очистки в роли worker.

Общие правила для подкоманд:

* У разрушительных команд нет общего флага подтверждения `--yes`. Если команда
  поддерживает `--dry-run`, сначала выполните этот предварительный просмотр,
  затем осознанно запустите команду с нужной областью действия.
* Контейнер worker запускает планировщик тех же задач очистки. Ручной вызов CLI
  не конфликтует с ним: оба используют одну таблицу аренды и не могут
  одновременно обработать одну строку.

## Когда что запускать?

Ниже указана стартовая команда для типичных симптомов.

| Симптом | Куда смотреть |
|---|---|
| «Диск кончается, что его съело?» | [`audit storage-summary`](#audit-storage-summary) |
| «Поиск работает медленно; возможно, проблема в индексах» | [`audit index-bloat`](#audit-index-bloat) |
| «Документ загрузился, но не появляется в ответах» | [`audit null-head-docs`](#audit-null-head-docs), затем [`repair null-heads`](#repair-null-heads) |
| «Webhook жизненного цикла попал в `dead-letter` и блокирует удаление каталога» | [`audit webhook-outbox`](#audit-webhook-outbox), затем повторная постановка через [`repair webhook-outbox-dead-letter`](#repair-webhook-outbox-dead-letter) или явное завершение через [`repair webhook-outbox-dead-letter-resolve`](#repair-webhook-outbox-dead-letter-resolve) |
| «Устаревшие векторы раздувают горячий ANN-индекс» | [`gc superseded-vectors`](#gc-superseded-vectors) |
| «`runtime_graph_evidence` больше остальной БД» | [`gc stale-evidence`](#gc-stale-evidence) |
| «Неудачная загрузка оставила много документов без головы» | [`repair null-heads-auto`](#repair-null-heads-auto) |
| «`ingest_stage_event` слишком старый, запросы замедлились» | [`retention stage-events`](#retention-stage-events) |
| «Сменили модель эмбеддингов библиотеки; векторы нужно перестроить» | [`rebuild vector-plane`](#rebuild-vector-plane) |
| «Нужно заново построить все векторы библиотеки» | [`rebuild vector-plane`](#rebuild-vector-plane) |
| «Граф разошёлся с документами» | [`rebuild runtime-graph`](#rebuild-runtime-graph) |
| «Нужно проверить фоновый планировщик» | [`lease summary`](#lease-summary) |
| «Задача очистки попала в `dead-letter`; причина устранена» | [`lease clear-failure`](#lease-clear-failure) |

## Сборка

```bash
cargo build --release -p ironrag-backend --bin ironrag-maintenance
```

Поставляется в Docker-образе по пути `/usr/local/bin/ironrag-maintenance`.

## Конфигурация

Использует те же переменные окружения, что и backend. Обязательная
database-настройка — `DATABASE_URL`.

## Развёртывание схемы

0.6.0 создаёт базу с нуля из одного baseline, `apps/api/migrations/0001_init.sql`. Базу 0.5.x нельзя обновить на месте: пересоздайте её и заново загрузите источники. SQLx проверяет контрольную сумму применённого baseline при старте, поэтому правка этого файла не переносит живую базу. `make migration-check` сравнивает исходный файл с `scripts/ops/baseline-checksum.txt` и ничего не меняет.

Проверенный операторский переход на существующей базе запускается как `ironrag-maintenance migrate <transition>`. Без `--apply` команда только показывает предусловия; `--apply` запускайте после резервной копии и с новым бинарём. Команда применяет свой DDL и обновляет записанную контрольную сумму. Не запускайте её SQL напрямую и не меняйте контрольную сумму вручную. Шаги, которым нужен `psql`, например `scripts/ops/apply-knowledge-structured-block-fk-indexes.sql`, остаются отдельными действиями оператора.

## `audit` — проверка без изменений

Подкоманды `audit` отвечают на вопрос «что происходит» и не изменяют данные.
Их безопасно запускать во время загрузки документов и поиска.

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
[webhook operations](./WEBHOOK.md#операции-с-dead-letter-lifecycle-outbox).

---

## `gc` — удаление мусора

Семейство gc удаляет производные данные, до которых больше нельзя дойти от
readable или подтверждённой leased active revision документа. Каждый gc subcommand берёт
per-library graph advisory lock и отказывается работать, пока в библиотеке
есть ingest job в состоянии `queued` / `leased` / `paused` — так
параллельный ingest не теряет данные из-за sweeper'а.

### `gc superseded-vectors`

**Что делает.** Удаляет chunk vectors из всех физических PostgreSQL
dimension-shards, если их revision не является readable или active revision,
которой владеют точный leased content-mutation job и его последняя leased
незавершённая attempt. Сам по себе stale active pointer вектор не защищает.
Source revisions, content chunks, knowledge chunks и сохранённые
query references остаются. При расхождении content-head и knowledge-head
операция останавливается fail-closed. Документы с двумя null-head по
умолчанию пропускаются.

Каждая delete-транзакция ограничена 10 000 строками. Фоновый scheduler
фиксирует не больше одного batch за запуск класса и ставит оставшуюся работу
на следующий tick. Durable keyset cursor идёт по существующему логическому
B-tree векторов и хранится в maintenance lease. При внешнем изменении
`source_truth_version` cursor сбрасывается, поэтому конкурентная смена head или
vector mutation не оставит новые stale-строки позади него.
Очистка graph/evidence продолжает работать в отдельном scheduler-only классе
`gc.runtime-graph`; его lease, retries и failures независимы от очистки
векторов. Этот класс за tick фиксирует не больше 10 000 физических
delete-мутаций, считая graph/evidence roots вместе с зависимыми ссылками
context bundle. Durable cursor хранит только текущую фазу очистки: на каждом
tick корни заново проверяются по актуальным evidence, затем блокируются и
удаляются только целой closure. Если одна неделимая closure сама больше
лимита, класс завершается явной ошибкой, а не превышает бюджет и не удаляет
часть citations. Лимит относится к зафиксированным delete-мутациям, но не к
query planning, чтению индексов или ожиданию locks. Commit удаления и update
lease cursor имеют at-least-once семантику: потеря ownership между ними
повторяет ту же идемпотентную фазу.

**Когда запускать.** Давление на PostgreSQL storage, особенно после
многих замен ревизий. Preview показывает логические строки; для
физического уменьшения HNSW/table нужен отдельный rebuild.

**Пример.**

```bash
# Безопасный preview: посчитать, не выполняя destructive-удаления.
ironrag-maintenance gc superseded-vectors --dry-run --json

# Реальный запуск, одна библиотека.
ironrag-maintenance gc superseded-vectors --library <uuid>

# Агрессивный режим: удалить также вектора null-head документов.
# Source и chunk history остаются доступными для repair.
ironrag-maintenance gc superseded-vectors --library <uuid> --include-null-head
```

### `gc stale-evidence`

**Что делает.** Удаляет строки `runtime_graph_evidence`, у которых
revision больше не readable/active head исходного документа, плюс
строки с `chunk_id` на чанк, которого больше нет. Оба lane пропускают строки, у которых документ
сейчас в активном ingest.

**Когда запускать.** Когда `audit storage-summary` показывает
`runtime_graph_evidence` как самую жирную таблицу, особенно после
revision-тяжёлого периода.

**Пример.**

```bash
ironrag-maintenance gc stale-evidence --library <uuid> --json
```

Пример вывода: `stale_revision_rows: 157645` означает, что 157k
строк в `runtime_graph_evidence` были привязаны к устаревшим
ревизиям. Спутник `phantom_chunk_rows` показывает строки,
привязанные к уже удалённым чанкам.

---

## `repair` — вернуть сломанный state к форме после успешного ingest

Семейство repair — это про *восстановление*, не про удаление. Оно
пишет новые строки, чтобы вернуть разъехавшиеся объекты к форме,
которую ingest pipeline создаёт при успешном завершении.

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
но per-document результат записывается на
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

### `repair knowledge-projection-metadata`

**Что делает.** Копирует расходящиеся метаданные из авторитетного content в
существующие knowledge-проекции того же workspace и library, только для активного
content с `deleted_at IS NULL`. Поля документа: `external_key`, `parent_document_id`,
`document_role`, `document_state`, `deleted_at`. Поля ревизии: `revision_number`,
`revision_kind`, `source_uri`, `document_hint`, `mime_type`, `checksum`, `title`,
`byte_size`. Tombstone на стороне knowledge не удаляет активный content:
repair восстанавливает проекцию в `active` с NULL в `deleted_at`.

**Границы.** Не меняет content, головы, readiness или chunks; не создаёт отсутствующие
проекции и не удаляет orphan-строки. Нет вызовов провайдера и планового backfill.
Применение использует существующую блокировку жизненного цикла library и повышает
source-truth generation только при изменении строк. `--dry-run` считает подходящие
расхождения без записи; повторный проход без новых расхождений сообщает ноль изменений.

```bash
ironrag-maintenance repair knowledge-projection-metadata --library <uuid> --dry-run --json
ironrag-maintenance repair knowledge-projection-metadata --library <uuid> --json
ironrag-maintenance repair knowledge-projection-metadata --library <uuid> --dry-run --json
```

Без `--library` обрабатываются все библиотеки.

---

## `retention` — TTL-чистка history-таблиц

Семейство retention удаляет строки из INSERT-only history-таблиц,
которые старше заданного срока хранения. Каждая
прогонка батчуется (10 000 строк на DELETE, 100 мс пауза между
батчами) — конкурентные ingest-writer'ы остаются отзывчивыми.

### `retention stage-events`

**Что делает.** Удаляет строки `ingest_stage_event` старше
`--older-than-days`. Поддерживающий индекс `idx_ingest_stage_event_recorded_at` входит в baseline
схемы, поэтому predicate компилируется в index range scan вместо seq scan под
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

**Что делает.** Повторно строит runtime-graph projection для одной
библиотеки или для всех. Batch-mode терпит
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
ironrag-maintenance lease show --class gc.superseded-vectors --json
ironrag-maintenance lease show --state dead_letter --json
```

### `lease summary`

**Что показывает.** Сводку по классу: pending / leased /
dead-letter. Компактный обзор state'а
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
ironrag-maintenance lease clear-failure --class gc.superseded-vectors --library <uuid>

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
docker exec <container> ironrag-maintenance gc superseded-vectors --dry-run
docker exec <container> ironrag-maintenance lease summary --json
```
