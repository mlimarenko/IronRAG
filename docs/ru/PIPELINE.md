# Пайплайн IronRAG

Документ описывает текущий канонический путь данных от admission источника до retrieval и выдачи ответа.

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

Один и тот же набор canonical services обслуживает web UI, HTTP handlers и MCP tools. Отдельного ingestion или query stack для агентов нет.

## 2. Каноническая нормализация источников

Любой принятый source сначала нормализуется в structured blocks. Только после этого запускаются chunking, embedding, graph extraction и retrieval.

### Поддерживаемые семейства источников

- Text-like файлы: markdown, text, JSON, YAML, source code
- PDF через text-layer extraction
- Изображения через multimodal description
- DOCX и PPTX через structured block extraction
- Таблицы (`csv`, `xlsx`, `xlsb`, `ods`) через row-oriented extraction
- Web pages через HTML main-content extraction

### Table contract

У таблиц один канонический путь:

- spreadsheet rows,
- extracted table blocks из office documents,
- extracted table blocks из поддерживаемых document parsers

все сходятся в один markdown-table representation плюс row-oriented normalized text. Retrieval и answering не держат отдельную spreadsheet-only ветку.

## 3. Модель хранения

### Postgres

Postgres хранит канонический control и content metadata:

- IAM, users, sessions, tokens, grants
- workspaces и libraries
- documents, revisions, heads, mutations и async operations
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

- entity types идут из канонического словаря из 10 типов
- relation types идут из канонического relation catalog
- `sub_type` — это metadata, а не node identity
- node identity строится из нормализованного `(node_type, label)`
- support count накапливается по admitted evidence

### Контракт entity resolution

Entity resolution схлопывает alias и normalized duplicate в один runtime vocabulary. Результат должен быть согласован между:

- query retrieval,
- graph topology,
- MCP graph tools,
- supporting document links.

## 6. Query и answer path

Query path использует один канонический retrieval stack:

- lexical retrieval
- vector retrieval
- evidence assembly
- canonical preflight answer preparation
- answer generation
- verification

Exact-literal technical вопросы используют тот же answer contract, но могут идти по lexical-only fast path, если вопрос явно про endpoint, parameter name или transport literal.

### Streaming contract

`POST /v1/query/sessions/{sessionId}/turns` с `Accept: text/event-stream` отдает:

- `runtime`
- `delta`
- `tool_call_started`
- `tool_call_completed`
- `completed`
- `error`

Финальный `completed` payload совпадает по shape с нестриминговым turn response.

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

Worker pool и HTTP API используют один и тот же canonical service layer и persistence model.

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

- Один канонический путь на каждое семейство источников; никаких alternate legacy branches.
- Одно каноническое table representation для всех форматов.
- Один канонический query pipeline для UI и MCP clients.
- Один канонический graph vocabulary для search, topology и relation listing.
- Никакой client-specific answer assembly логики вне query service.
