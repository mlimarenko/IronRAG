# Пайплайн IronRAG

Документ описывает текущий единый путь данных от admission источника до retrieval и выдачи ответа.

## 1. Точки входа

Content pipeline начинается с этих HTTP surface:

- `POST /v1/content/documents` для inline text и structured payload
- `POST /v1/content/documents/upload` для multipart file upload
- `POST /v1/content/documents/{documentId}/append`
- `POST /v1/content/documents/{documentId}/edit`
- `POST /v1/content/documents/{documentId}/replace`
- `POST /v1/content/web-runs` для single-page и recursive web ingestion

Query pipeline начинается с:

- `POST /v1/query/sessions/{sessionId}/turns`

Один и тот же набор сервисов обслуживает web UI, HTTP handlers и MCP tools. Отдельного ingestion или query stack для агентов нет.

## 2. Единая нормализация источников

Любой принятый source сначала нормализуется в structured blocks. Только после этого запускаются chunking, embedding, graph extraction и retrieval.

### Поддерживаемые семейства источников

- Text-like файлы: markdown, text, source code
- Structured-record файлы — JSON (объект или массив), YAML (один документ, `---` поток или последовательность mapping'ов), JSONL/NDJSON и TOML — через один key-agnostic record-экстрактор. Каждое поле на любой глубине вложенности разворачивается в searchable-текст, гетерогенные схемы (разные ключи в разных записях) профилируются, а любое значение, по виду похожее на timestamp (RFC3339 или epoch, под любым именем ключа), проставляет временную метку записи для temporal-retrieval. Никакого per-format или per-field спец-кейсинга: произвольный экспорт, лог событий, дамп конфига или транскрипт сессии идут через один общий путь.
- PDF через Docling-backed document-layout extraction с durable page-range checkpoints для stored revisions
- Статические raster images через Docling OCR по умолчанию или через активный `vision` binding, если recognition policy библиотеки выбирает `vision`
- DOCX и PPTX через Docling-backed structured block extraction
- Таблицы (`csv`, `tsv`, `xls`, `xlsx`, `xlsb`, `ods`) через native row-oriented extraction
- Web pages через HTML main-content extraction

### Recognition routing

Маршрут распознавания хранится как явная настройка библиотеки, а не как скрытый
runtime fallback. Новые библиотеки наследуют
`IRONRAG_RECOGNITION_DEFAULT_RASTER_IMAGE_ENGINE`; допустимые значения —
`docling` или `vision`, default — `vision`. Per-library обновление:
`PUT /v1/catalog/libraries/{libraryId}/recognition-policy`.

PDF, DOCX и PPTX layout extraction остаётся на embedded Docling CPU runtime.
Таблицы остаются на native tabular parser. Static raster image OCR и embedded
document-picture OCR идут через Docling, если библиотека явно не выбрала
`vision` в recognition policy. Если библиотека направляет image OCR в `vision`,
но binding не настроен, ingest падает явно, без silent fallback. Video files в
текущий ingest surface не входят.

Stored PDF revisions идут через restart-safe Docling path: worker сначала
читает page count, затем извлекает bounded page ranges и сохраняет каждый
завершённый range как ingest unit. `IRONRAG_DOCLING_PAGE_BATCH_SIZE` управляет
размером persisted range, `IRONRAG_DOCLING_PAGE_STREAM_WINDOW_PAGES` управляет
тем, сколько contiguous pages проходит через один Docling process (по умолчанию
40 страниц), а
`IRONRAG_DOCLING_MAX_CONCURRENCY` ограничивает локальные Docling процессы.
Уже завершённые page ranges переиспользуются после worker restart, backend
restart, потери lease или сетевого обрыва.
Перед запуском Docling process worker проверяет текущий hard memory headroom
cgroup против минимального бюджета одного процесса. Если один процесс не
помещается, документ завершается terminal ingest error
`docling_insufficient_memory`, без запуска Python и без SIGKILL/retry loop.
`IRONRAG_INGESTION_HEAVY_PIPELINE_PARALLELISM=auto` управляет тем, сколько
больших PDF pipelines могут быть активны до provider-bound стадий.
Автоматическое значение считается по CPU и memory cgroup лимитам worker-а и
по умолчанию ограничено 4; оно также ограничено настроенным параллелизмом
Docling subprocess, чтобы heavy jobs не копились без границы за
`IRONRAG_DOCLING_MAX_CONCURRENCY`.
У claim-loop для canonical ingest есть второй memory guard: перед claim новых
leases worker выводит максимум активных jobs для процесса из resolved cgroup
soft memory limit. Этот guard независим от deployment-wide global / workspace /
library caps и защищает небольшие swapless hosts от накопления нескольких
memory-heavy задач в одном worker process.

### Table contract

У таблиц один стандартный путь:

- spreadsheet rows,
- extracted table blocks из office documents,
- extracted table blocks из поддерживаемых document parsers

все сходятся в один markdown-table representation плюс row-oriented normalized text. Retrieval и answering не держат отдельную spreadsheet-only ветку.

## 3. Модель хранения

### PostgreSQL

PostgreSQL хранит control plane и knowledge plane:

- IAM, users, sessions, tokens, grants
- workspaces и libraries
- documents, revisions, heads, mutations, async operations и durable ingest units
- costs, audit events, runtime execution metadata
- structured blocks, chunks, technical facts, graph data, evidence, context bundles
- pgvector embeddings и PostgreSQL full-text search material

### Родительство документов

Документ, принятый как зависимый от другого источника (вложение страницы или
inline-картинка), записывает канонический `parent_document_id` и типизированную
роль `document_role` (`primary`, `attachment` или `attached_context`). Роль
решается один раз — при admission или per-library backfill'е родительства — из
структурных признаков: было ли объявлено родительство плюс media-class ревизии
(raster-image ребёнок становится `attached_context`; любой другой ребёнок —
peer `attachment`; без родителя — `primary`). Retrieval читает типизированную
роль и никогда не инспектирует MIME, расширение или имя файла. Роль зеркалится
на knowledge-plane строку документа, которую читает query-путь.

### Blob storage

Байты исходника лежат за `content_revision.storage_key` в настроенном storage backend.

## 4. Chunking

Chunking один для всех форматов:

- целевой размер: `2800` символов
- overlap: `280` символов
- heading-aware split
- code-aware split
- table-aware grouping
- near-duplicate suppression

Чанки строятся из structured blocks, а не напрямую из raw file.

## 5. Стадии enrichment

После нормализации и chunking IronRAG выполняет:

- embeddings
- technical fact extraction
- graph extraction
- document summary и quality signals

### Контракт graph extraction

- entity types идут из общего словаря из 10 типов
- relation types идут из общего relation catalog
- `sub_type` — это metadata, а не node identity
- node identity строится из нормализованного `(node_type, label)`
- support count накапливается по admitted evidence
- provider JSON чинится только для однозначного UTF-8 transport damage, затем
  валидируется до persistence; оставшиеся mojibake или control characters
  явно валят chunk

### Контракт graph key

Runtime graph nodes пишутся по одному key: нормализованный
`(node_type, label)`. Извлечённые aliases помогают lookup и relation endpoint
matching, но отдельного full-library alias resolution pass, который после
ingestion переписывает node identity, нет. Результат должен быть согласован между:

- query retrieval,
- graph topology,
- MCP graph tools,
- supporting document links.

## 6. Query и answer path

### Конфигурация retrieval на уровне библиотеки

Каждая библиотека хранит JSON-объект `retrieval_config`, управляющий
параметризацией поисковых лейнов. Конфигурация записывается в колонку
`catalog_library.retrieval_config` и доступна через
`GET /v1/catalog/libraries/{id}/retrieval-config` и
`PUT /v1/catalog/libraries/{id}/retrieval-config` (требуется разрешение
на запись в библиотеку).

**Текущие параметры** (отсутствующие ключи принимают значение по умолчанию):

| Путь ключа | Тип | По умолчанию | Эффект |
|---|---|---|---|
| `lexical.textSearchConfig` | string | `"simple"` | Имя конфигурации полнотекстового поиска PostgreSQL, используемое в лексическом лейне (вызовы `websearch_to_tsquery` и `to_tsquery`). Должно совпадать с записью в `pg_ts_config`; неизвестные имена отклоняются с HTTP 400. |

Значение по умолчанию `"simple"` воспроизводит историческое поведение
байт-в-байт: SQL, отрендеренный с дефолтной конфигурацией, побайтово совпадает
с исходной константой. Переключение на, например, `"english"` переводит
лексический лейн на английский словарь со стеммингом, что улучшает recall по
морфологически близким формам за счёт точного совпадения.

Конфигурация валидируется в момент записи: бэкенд запрашивает `pg_ts_config` и
отклоняет имена конфигураций, отсутствующие в базе данных. Это отлавливает
опечатки до того, как они незаметно деградируют retrieval.

Query path использует единый retrieval stack:

- lexical retrieval
- vector retrieval
- evidence assembly
- preflight answer preparation
- answer generation
- verification

Планировщик lexical lane выводит high-level и low-level seeds из
скомпилированного `QueryIR`, а не из позиции keyword'ов. Subject/object
entities, target types, document focus, comparison operands и exact literals
попадают в high-level lane; modifiers, comparison dimensions, temporal
constraints и source-slice refinements попадают в low-level lane. Если IR
отсутствует, low-confidence, не даёт seed'ов или не совпадает с extracted
keywords, оба lane используют полный набор extracted keywords, сохраняя прежнее
lexical behavior.

Exact-literal technical вопросы используют тот же answer contract, но могут идти по lexical-only fast path, если вопрос явно про endpoint, parameter name или transport literal.

У setup и versioned procedure вопросов есть дополнительные structural
lanes до свободной генерации ответа. Broad setup запрос может вернуть
детерминированный setup-variants ответ, если retrieved документы дают grounded
якоря item, command, path, section или parameter
для нескольких правдоподобных вариантов. Запрос, уже сфокусированный на одном
документе или subject, остаётся на focused path и не расширяется
multi-variant shortcut'ом. Versioned procedure вопросы строят subject/acronym
profile из typed query plan и document labels, затем требуют ordered procedure
evidence, прежде чем transition document сможет обойти generic release notes
или compatibility pages. Точные instruction-title якоря
procedure защищены при truncation retrieval-контекста и при topical pruning,
который вычищает generic tails. Rerank может поднять обычные relevance chunks,
но не снижает абсолютные scores защищённых evidence lanes: document identity,
focused document, query-IR focus и procedure anchors. Если запрос типизирован
как update procedure, inferred latest-version inventory fallback отключается,
чтобы списки релизов или changelog'и не перехватывали детерминированный
procedure answer. Transport assignment rendering остаётся отдельным: нужен
typed port/protocol/connection intent и конкретные `name = value` evidence;
typed service/port inventory без connection-сигнала остаётся на обычном
synthesis path.

Документы с типизированной ролью `attached_context` (raster-image вложения родительской страницы) — это подчинённый контекст, а не конкурирующие peer-документы: их чанки демоутятся ниже peer- и primary-контента при финальном отборе контекста, исключаются из clarify-vs-answer диспозиции и никогда не становятся clarify-вариантом. Поэтому однонёночные image-вложения страницы не могут заполонить контекст ответа или меню уточнения и вытеснить evidence самой родительской страницы. Исключение — запрос, явно сфокусированный на самом вложении: оно остаётся в primary-полосе.

Если retrieval показывает, что вопрос без явного субъекта совпадает с несколькими разными субъектами, ответ начинается с grounded-фрагмента по одному из них и уточняет, на каком субъекте сфокусироваться. Список вариантов строится из данных самой библиотеки (группировка документов или entity-evidence графа знаний), а не из захардкоженных списков. Это касается и детерминированного latest-version inventory пути: release-inventory вопрос без scope-субъекта уточняет, когда перечисленные release-документы упоминают несколько разных субъектов графа, а запрос, ограниченный entity, document focus или literal, сохраняет плоский список последних версий.

### Turn contract

`POST /v1/query/sessions/{sessionId}/turns` создаёт один persisted assistant
turn и query execution. UI callers могут запросить `text/event-stream`; stream
несёт activity, failure и completion events для того же execution, а completion
payload содержит grounded answer, evidence references, verifier state и runtime
execution handle. Если transport падает после старта backend work, frontend
восстанавливается чтением durable session result, созданного после request
boundary, вместо повторной отправки turn. MCP transport streaming остаётся
изолированным в `/v1/mcp`.

## 7. Worker model

Фоновая обработка lease-based и stage-driven. Worker отвечает за:

- content extraction
- structure preparation
- chunk processing
- embeddings
- technical facts
- graph extraction
- verification
- finalization
- web discovery и page materialization

Worker pool и HTTP API используют один и тот же service layer и persistence model.
Каждый claimed job получает отдельный heartbeat observer, поэтому долгие
provider или Docling calls не могут заморить lease renewal. Если lease ушёл
другому worker'у, pipeline останавливается, job поднимается из durable state,
а finalization проверяет active attempt lease вместо stale in-memory success flag.

## 8. Бэкап и восстановление библиотеки

Библиотеку можно экспортировать в самодостаточный `.tar.zst` архив и восстановить на том же или другом деплойменте IronRAG.

### Экспорт

```
GET /v1/content/libraries/{id}/snapshot?include=library_data,blobs
```

Ответ стримит tar-архив со zstd-сжатием. Содержимое:

- `manifest.json` — версия схемы, id библиотеки, scope включений
- `postgres/<table>/part-NNNNNN.ndjson` — строки таблиц (макс. 64 МiB на часть)
- `blobs/<storage_key>` — оригинальные файлы (опционально через `blobs`)
- `summary.json` — подсчёт строк при экспорте

`include=library_data` включает PostgreSQL library data, включая строки
knowledge plane. `blobs` добавляет загруженные файлы. Фронтенд использует
`<a href>` без буферизации в JS.

### Импорт

```
POST /v1/content/libraries/{id}/snapshot?overwrite=reject|replace
Content-Type: application/zstd
Body: raw .tar.zst архив
```

Импорт читает manifest из архива. `overwrite=replace` очищает существующие
данные перед вставкой. PostgreSQL строки вставляются batch'ами по 1000 через
`jsonb_populate_recordset`. Новые экспорты используют snapshot schema v7.
Restore path принимает PostgreSQL-only v6 и v7 archives; legacy v5 archives
из 0.4.x больше не поддерживаются.

## 9. Жесткие инварианты

- Один стандартный путь на каждое семейство источников; никаких alternate legacy branches.
- Одно table representation для всех форматов.
- Один общий query pipeline для UI и MCP clients.
- Один общий graph vocabulary для search, topology и relation listing.
- Никакой client-specific answer assembly логики вне query service.
