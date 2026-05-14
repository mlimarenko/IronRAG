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

- Text-like файлы: markdown, text, JSON, YAML, source code
- PDF через Docling-backed document-layout extraction с durable page-range checkpoints для stored revisions
- Статические raster images через Docling OCR по умолчанию или через активный `vision` binding, если recognition policy библиотеки выбирает `vision`
- DOCX и PPTX через Docling-backed structured block extraction
- Таблицы (`csv`, `tsv`, `xls`, `xlsx`, `xlsb`, `ods`) через native row-oriented extraction
- Web pages через HTML main-content extraction

### Recognition routing

Маршрут распознавания хранится как явная настройка библиотеки, а не как скрытый
runtime fallback. Новые библиотеки наследуют
`IRONRAG_RECOGNITION_DEFAULT_RASTER_IMAGE_ENGINE`; допустимые значения —
`docling` или `vision`, default — `docling`. Per-library обновление:
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
`IRONRAG_INGESTION_HEAVY_PIPELINE_PARALLELISM=auto` управляет тем, сколько
больших PDF pipelines могут быть активны до provider-bound стадий.
Автоматическое значение считается по CPU и memory cgroup лимитам worker-а и
по умолчанию ограничено 4; оно также ограничено настроенным параллелизмом
Docling subprocess, чтобы heavy jobs не копились без границы за
`IRONRAG_DOCLING_MAX_CONCURRENCY`.

### Table contract

У таблиц один стандартный путь:

- spreadsheet rows,
- extracted table blocks из office documents,
- extracted table blocks из поддерживаемых document parsers

все сходятся в один markdown-table representation плюс row-oriented normalized text. Retrieval и answering не держат отдельную spreadsheet-only ветку.

## 3. Модель хранения

### Postgres

Postgres хранит основной control и content metadata:

- IAM, users, sessions, tokens, grants
- workspaces и libraries
- documents, revisions, heads, mutations, async operations и durable ingest units
- costs, audit events, runtime execution metadata

### Blob storage

Байты исходника лежат за `content_revision.storage_key` в настроенном storage backend.

### ArangoDB

Arango хранит structured document и graph material, которые используются ingestion, retrieval и topology API. Это runtime data surface для graph-oriented read-path и staged extraction artifacts.

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

Query path использует единый retrieval stack:

- lexical retrieval
- vector retrieval
- evidence assembly
- preflight answer preparation
- answer generation
- verification

Exact-literal technical вопросы используют тот же answer contract, но могут идти по lexical-only fast path, если вопрос явно про endpoint, parameter name или transport literal.

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
- `arango/<collection>/part-NNNNNN.ndjson` — документы знаний
- `arango-edges/<collection>/part-NNNNNN.ndjson` — связи знаний
- `blobs/<storage_key>` — оригинальные файлы (опционально через `blobs`)
- `summary.json` — подсчёт строк при экспорте

`include=library_data` включает все данные Postgres и Arango. `blobs` добавляет загруженные файлы. Фронтенд использует `<a href>` — без буферизации в JS.

### Импорт

```
POST /v1/content/libraries/{id}/snapshot?overwrite=reject|replace
Content-Type: application/zstd
Body: raw .tar.zst архив
```

Импорт читает manifest из архива. `overwrite=replace` очищает существующие данные перед вставкой. Postgres строки вставляются batch'ами по 1000 через `jsonb_populate_recordset`. Arango — bulk AQL INSERT.

## 9. Жесткие инварианты

- Один стандартный путь на каждое семейство источников; никаких alternate legacy branches.
- Одно table representation для всех форматов.
- Один общий query pipeline для UI и MCP clients.
- Один общий graph vocabulary для search, topology и relation listing.
- Никакой client-specific answer assembly логики вне query service.
