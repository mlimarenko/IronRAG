# IronRAG: пайплайн обработки контента

Документ описывает полный путь данных от загрузки источника до построения графа знаний и retrieval. Цель — иметь одно место, где зафиксированы все нюансы по разным типам файлов, web-ingestion, чанкингу, экстракции, merge и dedup, чтобы не пересобирать карту по коду каждый раз.

Все ссылки на код даны в формате `path:line`, относительно `ironrag/`.

---

## 1. Точки входа (HTTP)

Все REST-роуты определены в `apps/api/src/interfaces/http/`.

| Источник | Метод + путь | Handler |
|---|---|---|
| Inline-документ (text/json) | `POST /content/documents` | `interfaces/http/content.rs` (`create_document`) |
| Загрузка файла (multipart) | `POST /content/documents/upload` | `interfaces/http/content.rs` (`upload_inline_document`) |
| Append/edit/replace | `POST /content/documents/{id}/append|edit|replace` | `interfaces/http/content.rs` |
| Web ingestion (single + crawl) | `POST /content/web-runs` | `services/ingest/web.rs` |
| Статус ingest jobs | `GET /ingest/jobs`, `GET /ingest/attempts/{id}` | `interfaces/http/ingestion.rs` |

`POST /content/documents/upload` парсит multipart, извлекает `file_name`, `mime_type`, `file_bytes` и собирает `UploadInlineDocumentCommand`, который дальше уходит в `ContentService::upload_inline_document` (`services/content/service/pipeline.rs:152`).

---

## 2. Файловые типы и парсеры

Тип файла определяется в `services/shared/extraction/file_extract.rs` через `detect_upload_file_kind()` (по расширению + MIME fallback).

| `UploadFileKind` | Расширения / MIME | Парсер | Особенности |
|---|---|---|---|
| `TextLike` | `txt`, `md`, `json`, `yaml`, исходники (rs, py, ts, js, …) | `services/shared/extraction/file_extract/normalization.rs` | Нормализация переносов, decode UTF-8/BOM, базовая очистка |
| `Pdf` | `application/pdf` | `services/shared/extraction/pdf.rs` (Pdfium) | Извлечение текста по страницам, попытка восстановления layout. **OCR отсутствует** — отсканированные PDF без текстового слоя выпадают |
| `Image` | `png`, `jpg`, `jpeg`, `gif`, `webp`, `svg` | Vision LLM (через `LlmGateway`) | Описание изображения через мультимодальную модель. Это **не** OCR, а семантическое описание |
| `Docx` | `application/vnd.openxmlformats-…wordprocessingml…` | `services/shared/extraction/docx.rs` | Структурированные блоки (параграфы, заголовки, таблицы) |
| `Spreadsheet` | `csv`, `xlsx`, `ods` | `services/shared/extraction/spreadsheet.rs` | Каждая строка → отдельный structured block c kind=`table_row` |
| `Pptx` | `application/vnd.openxmlformats-…presentationml…` | `services/shared/extraction/pptx.rs` | Слайды → блоки |
| `Binary` | прочее | — | Отклоняется на admission stage |

Все парсеры выдают единое представление: `Vec<StructuredBlockData>`. Это нужно потому, что **chunking, embedding и graph extraction знают только о структурированных блоках**, а не о конкретном формате файла.

---

## 3. Web ingestion

`POST /content/web-runs` создаёт `CreateWebIngestRunCommand` → `services/ingest/web.rs`.

Две стратегии:

- **Single page** — `services/ingest/web/single_page.rs`. Один fetch, readability extraction.
- **Recursive crawl** — `services/ingest/web/recursive.rs`. BFS с boundary policy: same-domain / subdomain limit, max depth, max pages.

**Fetch:** через reqwest с timeout 20s, max 10 redirects, кастомный User-Agent.

**Извлечение контента:** `services/shared/extraction/html_main_content.rs`:
- `extract_html_canonical_url()` — нормализация URL (canonical link, дедупликация по URL).
- Readability-style boilerplate removal — выкидывает nav, footer, sidebar.
- Возвращает structured blocks той же формы, что и парсеры файлов.

**PDF по ссылке:** detect по `Content-Type`, скачивается, дальше идёт через тот же `pdf.rs`.

**Не поддерживается специально:** GitHub repo (никакого clone'а), YouTube transcripts, API docs (обрабатываются как обычный HTML).

**Дедупликация:** по canonical URL — повторный fetch того же URL не создаёт новый `content_document`.

---

## 4. Модель хранения

### Postgres-таблицы (`apps/api/migrations/0001_init.sql`)

```
catalog_library (id, …, extraction_prompt)
    │
    ├──> content_document (id, library_id, external_key, document_state)
    │       │
    │       ├──> content_revision (id, document_id, revision_number, mime_type,
    │       │       byte_size, title, source_uri, storage_key, parent_revision_id)
    │       │       │
    │       │       └──> content_chunk (id, revision_id, chunk_index,
    │       │               normalized_text, text_checksum, token_count, …)
    │       │
    │       └──> content_document_head (document_id → active_revision_id)
    │
    ├──> runtime_graph_node (line 1028)
    │       (id, library_id, canonical_key, label, node_type,
    │        aliases_json, summary, metadata_json, support_count,
    │        projection_version, …)
    │
    └──> runtime_graph_edge (line 1046)
            (id, from_node_id, to_node_id, relation_type, canonical_key,
             support_count, metadata_json, projection_version, …)
```

**Версионирование контента:** `revision_number` инкрементируется, `parent_revision_id` строит цепочку. `content_document_head.active_revision_id` указывает на текущую версию.

**Хранение исходных байт:** `content_revision.storage_key` → blob storage (S3-like, реализация в `infra/storage/`).

**Versionирование графа:** `projection_version` (bigint). Каждый rebuild создаёт новый `runtime_graph_snapshot`, граф читается по `(library_id, projection_version)`. Активная версия — `active_projection_version()` (`services/graph/projection.rs:83`).

### Arango-stores

Структурированные блоки и kandidaty graph extraction до промоушена в Postgres хранятся в Arango (`arango_document_store`, `arango_graph_store`). Постоянная истина — Postgres `runtime_graph_*`, Arango — это staging.

---

## 5. Chunking

`services/shared/extraction/chunking.rs`, профиль `StructuredChunkingProfile`:

```rust
StructuredChunkingProfile {
    max_chars: 2_800,
    overlap_chars: 280,   // ~10% overlap
}
```

Алгоритм `build_structured_chunk_windows`:
- Идёт по `Vec<StructuredBlockData>` в порядке исходного документа.
- **Heading-aware:** заголовок открывает новый chunk.
- **Table-aware:** строки таблиц либо группируются в один chunk, либо каждая идёт отдельно с `chunk_kind="table_row"` (для маленьких таблиц).
- **Code-aware:** большие code-блоки pre-split на семантических границах.
- **Overlap:** последние блоки предыдущего chunk дублируются в начало следующего.
- **Near-duplicate detection:** `mark_near_duplicates()` через simhash, чтобы не плодить чанки по идентичным секциям.

Чанки сохраняются как `content_chunk` рядов, каждый получает `text_checksum` (SHA256 нормализованного текста). Этот checksum дальше используется в **diff-aware ingest** — см. секцию 9.

---

## 6. Embedding

- **Модель:** конфигурируется через provider catalog. Default seed в миграциях — `text-embedding-3-large` (OpenAI).
- **Хранение:** pgvector column (точное имя зависит от миграции). Доступ через `services/query/search.rs`.
- **Когда считается:** **async**, отдельная стадия job pipeline `embed_chunk` (см. секцию 8). Не блокирует ingestion.
- **Использование:** гибридный retrieval (vector top-K + graph evidence).

---

## 7. Graph extraction (главная стадия)

### 7.1. Стадии job pipeline

`services/ingest/service.rs:21-30`:

```
extract_content       → вытащить текст из файла/URL
prepare_structure     → собрать structured blocks
chunk_content         → нарезать на chunks
embed_chunk           → async embedding
extract_technical_facts
extract_graph         ← вот здесь
verify_query_answer
finalizing
web_discovery / web_materialize_page  (для web ingest)
```

Каждая стадия — отдельный `IngestJob` с lease-based attempt'ом (`services/ingest/worker.rs`).

### 7.2. Что отдаётся в LLM

Тип запроса — `GraphExtractionRequest` (`services/graph/extract/types.rs:14-25`):

```rust
pub struct GraphExtractionRequest {
    pub library_id: Uuid,
    pub document: DocumentRow,
    pub chunk: ChunkRow,
    pub structured_chunk: GraphExtractionStructuredChunkContext,
    pub technical_facts: Vec<GraphExtractionTechnicalFact>,
    pub revision_id: Option<Uuid>,
    pub activated_by_attempt_id: Option<Uuid>,
    pub resume_hint: Option<GraphExtractionResumeHint>,
    pub library_extraction_prompt: Option<String>,
    pub sub_type_hints: GraphExtractionSubTypeHints,
}
```

`GraphExtractionSubTypeHints` определён в `types.rs:32-52` как `{ by_node_type: Vec<GraphExtractionSubTypeHintGroup> }` с методом `is_empty()`. Поле **не Option** — пустое значение (`GraphExtractionSubTypeHints::default()`) корректно, оно просто рендерится как ничего и секция в промпт не попадает.

Конструируется в `services/content/service/pipeline.rs:68` (`build_canonical_graph_extraction_request`), вызывается из `services/content/service/revision.rs:1082` per-chunk внутри parallel stream.

### 7.3. Структура промпта

`services/graph/extract/prompt.rs:70` — `build_graph_extraction_prompt_plan()`. Промпт собирается как набор именованных секций `[name]\nbody`, в порядке:

1. **`task`** (`prompt.rs:90`) — главная инструкция: extract entities + relations, resolve coreferences, prefer specific relation types over `mentions`.
2. **`entity_types`** (`prompt.rs:107`) — **жёстко зашитый статический список** из 10 типов: `person`, `organization`, `location`, `event`, `artifact`, `natural`, `process`, `concept`, `attribute`, `entity`. Никакой динамической подгрузки.
3. **`examples`** (`prompt.rs:121`) — два примера (API docs + infrastructure).
4. **`schema`** (`prompt.rs:130`) — JSON schema requirement. Тут же явно указано, что `sub_type` — **freeform specialization** (framework, database, algorithm, …). `relation_type` берётся из `canonical_relation_type_catalog()` (`services/graph/identity.rs`).
5. **`rules`** (`prompt.rs:137`) — критические правила: no markdown fences, no empty summaries, no `mentions` when something specific fits.
6. **`document`** (`prompt.rs:146`) — document label + chunk ordinal.
7. **`domain_context`** (`prompt.rs:156`) — section path.
8. **`library_context`** (`prompt.rs:162-167`) — опциональная per-library инструкция из `catalog_library.extraction_prompt`.
9. **`sub_type_hints`** (`prompt.rs:168-170`) — список наблюдённых `sub_type` per `node_type` для текущей библиотеки, с инструкцией «prefer existing if applicable». Секция выводится только если hints непустой. Renderer — `render_sub_type_hints` (`prompt.rs:293`). См. секцию 14.
10. **`structured_chunk`** (`prompt.rs:167`) — chunk kind, section path, heading trail, support block count.
11. **`technical_facts`** (`prompt.rs:171`) — отрендеренные typed facts (если есть).
12. **`downgrade`** (`prompt.rs:177`) — если идёт recovery с downgrade.
13. **`recovery`** + **`previous_output`** (`prompt.rs:186-207`) — для retry attempts.
14. **`chunk_segment_*`** — собственно текст чанка, нарезанный на 1-3 сегмента в зависимости от downgrade level.

### 7.4. Adaptive downgrade

При повторных fail'ах extraction (`resume_hint.downgrade_level > 0`) промпт ужимается:
- `downgrade_level=1`: лимит размера / 2, max 2 сегмента chunk text.
- `downgrade_level=2`: лимит / 3, max 1 сегмент.

Это даёт LLM второй шанс на проблемных чанках с урезанным контекстом.

### 7.5. Парсинг ответа

`services/graph/extract/parse.rs`:
- `extract_json_payload()` — вытаскивает JSON из ответа (терпим к ```json fences).
- `parse_entity_candidate()` — парсит entity (`parse.rs:222-228` для `sub_type`):
  ```rust
  let sub_type = value.get("sub_type")
      .and_then(serde_json::Value::as_str)
      .map(str::trim)
      .filter(|s| !s.is_empty())
      .map(ToString::to_string);
  ```
- `parse_relation_candidate()` — парсит relation, валидирует `relation_type` против каталога.
- `refine_entity_type()` — пост-обработка: исправляет очевидно неверные типы по эвристикам.

Выход — `GraphExtractionCandidateSet { entities, relations }`.

---

## 8. Merge / upsert в граф

### 8.1. Точка входа

`services/graph/merge.rs:130` — `merge_chunk_graph_candidates()`. Принимает `GraphMergeScope` (`library_id`, `projection_version`, `revision_id`, `attempt_id`), документ, chunk, кандидатов.

### 8.2. Identity ключ

`services/graph/identity.rs`:
- `canonical_node_key(node_type, label)` → `"{node_type_slug}:{normalized_label}"`.
- `normalize_graph_identity_component()` — lowercase, trim, Unicode NFKC, удаление пунктуации.

**`sub_type` НЕ участвует в identity key.** Два кандидата с одинаковым `(node_type, label)`, но разными `sub_type`, схлопываются в один `runtime_graph_node`.

### 8.3. Upsert узла

`services/graph/merge.rs:457-504` — `upsert_graph_node()`:

```rust
let canonical_key = canonical_node_key(node_type, label);
let existing = get_runtime_graph_node_by_key(pool, library_id, canonical_key, projection_version);
let support_count = existing.map_or(1, |row| row.support_count.max(1));

let mut metadata = merge_graph_quality_metadata(
    existing.map(|row| &row.metadata_json),
    extraction_recovery,
    summary,
);
if let Some(st) = sub_type {
    metadata.as_object_mut().map(|obj| {
        obj.insert("sub_type".to_string(), Value::String(st.to_string()))
    });
}

upsert_runtime_graph_node(...)
```

**Поведение полей при конфликте:**

| Поле | Политика |
|---|---|
| `aliases_json` | Union (нормализация + дедуп) |
| `summary` | Last-wins, если новое непустое |
| `support_count` | `max(existing, 1)` + инкремент через reconcile pass |
| `metadata_json.sub_type` | **Last-wins (перезапись)**. Старое значение теряется |
| прочая `metadata_json` | Merge через `merge_graph_quality_metadata()` |

### 8.4. Upsert ребра

`services/graph/merge.rs:506` — `upsert_graph_edge()`. Identity по `(from_canonical, relation_type, to_canonical)`. Аналогичная reconciliation для `support_count` через `reconcile_merge_support_counts()`.

---

## 9. Diff-aware ingest (re-use)

`services/content/service/revision.rs:982` — `build_chunk_reuse_plan()`.

При создании новой revision документа: для каждого нового chunk проверяется, существует ли в **родительской** revision chunk с тем же `text_checksum`. Если да — берутся уже существующие `runtime_graph_extraction` records, копируются с новым `chunk_id`, **LLM не вызывается**.

Это критическая оптимизация для документов, которые редактируются по частям — большинство чанков остаётся без изменений и переиспользует прошлую graph extraction.

---

## 10. Entity resolution (post-hoc dedup)

`services/graph/entity_resolution.rs`.

**Триггер:** `resolve_after_ingestion()` запускается после ingestion, когда в библиотеке ≥ 50 узлов (порог для эффективности).

**Алгоритмы match (детерминированные, без LLM, без embedding):**
1. **ExactAlias** — label одного узла точно совпадает с alias другого.
2. **NormalizedPrefix** — после strip известных суффиксов (`_database`, `_db`, `_framework`, `_system`, …).
3. **Acronym** — таблица known abbreviations (`pg` ↔ `postgresql`, `k8s` ↔ `kubernetes`, `jwt` ↔ `json web token`).

**Merge процесс:**
- Один узел — keep, второй — remove.
- Edges remove'а перенаправляются на keep.
- Label remove'а добавляется в `aliases_json` keep'а.
- `support_count` суммируется.

**Что теряется:** `metadata_json` удаляемого узла (включая `sub_type`!) **не переносится**. Это известная асимметрия с upsert path — там last-wins, тут silent loss. Sub_type hints (секция 14) бьют в корневую причину — чтобы LLM сразу выдавал конвергирующиеся значения, а не лечить пост-фактум.

---

## 11. Retrieval

`services/query/search.rs` + `services/query/execution/retrieve.rs`.

**Гибридная схема:**
- **Vector search** — pgvector similarity по embedding'ам чанков.
- **Graph search** — обход `runtime_graph_node` / `runtime_graph_edge` по найденным entities.
- **Fusion ranking** — комбинированный скор по relevance, support_count, метаданным.

**`sub_type` в retrieval НЕ участвует** — это чисто аннотативное поле в `metadata_json`, не индексируется и не фильтруется.

---

## 12. Job runner и resilience

### 12.1. Lease lifecycle

`services/ingest/worker.rs` — lease-based worker pool:
- `AdmitIngestJobCommand` создаёт `IngestJob` в `queue_state='queued'`.
- `claim_next_queued_ingest_job` (`jobs.rs`) атомарно переводит `queued → leased` с `for update skip locked` — несколько воркеров не конкурируют за одну запись. CTE `active_leases` считает **все** leased-job'ы против global / workspace / library cap'ов; предыдущий фильтр по freshness `heartbeat_at` вводил TOCTOU, позволявший свежим claim'ам обходить per-library cap, поэтому убран (зомби-lease'ы чистит reaper, а claim-query должна только соблюдать лимиты).
- `LeaseAttemptCommand` создаёт `ingest_attempt` в `attempt_state='leased'` + стартовый `heartbeat_at=now()`.
- `HeartbeatAttemptCommand` каждые `settings.ingestion_worker_heartbeat_interval_seconds` (default 15s) обновляет `heartbeat_at`.
- `FinalizeAttemptCommand` фиксирует `success | failed | canceled` + `failure_class` / `failure_code` / `retryable`.
- `extraction_recovery.rs` — логика retry с adaptive downgrade.

Параллелизм двумерный:
- **Cross-document (dispatcher)** — `settings.ingestion_max_parallel_jobs_per_library` (default 16) — статический потолок. Фактическая concurrency дополнительно снижается под давлением памяти через `ingestion_memory_soft_limit_mib`: перед каждым claim dispatcher читает worker RSS и не запускает новый job, если процесс превысил soft limit. Soft limit авто-резолвится из cgroup (или `/proc/meminfo`) к 90% памяти контейнера, когда config = `0` (`shared::telemetry::resolve_memory_soft_limit_mib`).
- **Per-document (graph extract fan-out)** — `settings.ingestion_graph_extract_parallelism_per_doc` (default 8) управляет `buffer_unordered`-конкурентностью per-chunk graph-extract LLM-вызовов внутри одного job. Независим от cross-doc лимита, чтобы тяжёлые документы получали полный chunk parallelism без увеличения cross-doc давления.

### 12.2. Stale lease reaper (periodic)

`services/ingest/worker/runtime.rs` — `run_canonical_lease_recovery_loop`:
- Раз в `CANONICAL_LEASE_RECOVERY_INTERVAL = 15s` находит attempts с `queue_state='leased'` + `attempt_state='leased'` + `heartbeat_at < now() - CANONICAL_STALE_LEASE_SECONDS (60s)` и возвращает job в очередь (`jobs.rs` — `recover_stale_canonical_leases`).
- Attempt помечается `attempt_state='failed'`, `failure_class='lease_expired'`, `failure_code='stale_heartbeat'`, `retryable=true`.
- Ловит сценарий "провайдер завис на минуты" — LLM-вызов держит task, но heartbeat тикать не может, и через 60s reaper возвращает job в queue, следующий воркер делает retry.

### 12.x. LLM transport retry schedule

Retryable provider failures (таймауты, transient 4xx/5xx — 408, 409, 425, 429, 500, 502, 503, 504, 520–524, 529 — плюс `reqwest` transport errors) retry'ятся по фиксированному расписанию: **1с, 3с, 10с, 30с, 90с** (`TRANSPORT_RETRY_SCHEDULE_SECS` в `integrations/llm/streaming.rs`). `llm_transport_retry_attempts` default 5 совпадает с длиной расписания; `runtime_graph_extract_recovery_max_attempts` default 4 добавляет внешний retry-слой вокруг per-chunk graph extraction, поэтому chunk переживает до 4 × (134 секунды лестницы backoff) transient-сбоев провайдера перед тем как всплыть как error.

### 12.3. Startup lease sweep (одноразовый на старте worker pool)

`services/ingest/worker/runtime.rs` — `reclaim_orphaned_leases_on_startup`, вызывается в `run_ingestion_worker_pool` **до того как dispatcher начинает брать новые job'ы**:

```rust
pub(super) async fn run_ingestion_worker_pool(...) {
    ...
    reclaim_orphaned_leases_on_startup(&state).await;
    let lease_recovery_handle = tokio::spawn(run_canonical_lease_recovery_loop(...));
    ...
}
```

Использует **более короткий** порог — `CANONICAL_STARTUP_LEASE_RECOVERY_SECONDS = 30s`. Обоснование: при старте процесса мы знаем, что сами leases не держим, поэтому любой `leased` row в БД либо принадлежит живому sibling-воркеру, либо осиротел. 30s = 2× heartbeat interval — здоровый sibling такой gap не допустит, орфанов ловим почти сразу.

**Что закрывает:** после рестарта backend / worker / всего стека документы, которые были в обработке, не висят "в processing" до минуты. Startup sweep возвращает их в очередь в первые секунды бут-процесса, и следующий цикл `claim_next_queued_ingest_job` берёт их в работу.

Видно в логах при старте:
```
WARN  startup lease sweep: reclaimed orphaned canonical ingest leases after worker pool boot
      recovered=9 threshold_seconds=30
```

### 12.4. Cancel flow (включая активный lease)

`cancel_jobs_for_document` (`infra/repositories/ingest_repository/jobs.rs:601` — бывший `cancel_queued_jobs_for_document`, переименован канонически) — **единственный** SQL cancel:

```sql
UPDATE ingest_job
SET queue_state = 'canceled', completed_at = now()
WHERE mutation_id IN (...)
  AND queue_state IN ('queued', 'leased')
  AND completed_at IS NULL
```

Покрывает **и** queued **и** leased. Для queued это атомарный терминал. Для leased установка `queue_state='canceled'` — это **сигнал воркеру**, который работает вместе с cooperative abort в пайплайне.

**Observer на heartbeat loop** (`worker.rs:execute_canonical_ingest_job`):
- Рядом с heartbeat task создаётся `JobCancellationToken { canceled: Arc<AtomicBool> }`.
- На каждом тике heartbeat loop, после записи `heartbeat_at`, делает `get_ingest_job_by_id`; если видит `queue_state='canceled'`, зажигает `token.mark_canceled()` и выходит из loop.
- Pre-lease guard: сразу после создания attempt читает queue_state один раз, чтобы поймать cancel, произошедший между claim'ом и первым heartbeat tick'ом.
- Latency: ≤ `ingestion_worker_heartbeat_interval_seconds` (default 15s) от момента `UPDATE queue_state='canceled'` до наблюдения в воркере.

**Pipeline guards** (`worker.rs:run_canonical_ingest_pipeline`):

```rust
async fn run_canonical_ingest_pipeline(..., cancellation: &JobCancellationToken) {
    cancellation.check(job.id)?;          // extract_content
    ...
    cancellation.check(job.id)?;          // prepare_structure / chunk_content / …
    ...
    cancellation.check(job.id)?;          // embed_chunk
    ...
    cancellation.check(job.id)?;          // extract_graph
    ...
    cancellation.check(job.id)?;          // finalize readiness
    ...
}
```

Между стейджами вставлены вызовы `cancellation.check(job_id)`. Если флаг поднят, возвращается `anyhow::Error::new(JobCanceledByRequest { job_id })` и pipeline останавливается. Мид-стейдж (во время LLM-вызова) cancel ждёт окончания текущего стейджа — приемлемо, LLM-вызовы ограничены провайдерским timeout.

**Finalize branch** (`worker.rs:execute_canonical_ingest_job`, `Err` ветка):
- Проверка `error.downcast_ref::<JobCanceledByRequest>()` идёт **первой**.
- Finalize с `attempt_state='canceled'`, `failure_class='content_mutation'`, `failure_code='canceled_by_request'`, `retryable=false`.
- Возвращается `Ok(())`, чтобы outer handler НЕ позвал `fail_canonical_ingest_job`.

**Fail handler guard** (`worker/failure.rs:44`):
- `fail_canonical_ingest_job` пропускает `queue_state IN ('completed', 'canceled')` — защита от race, если где-то ошибка JobCanceledByRequest не успеет downcastнуться, пользовательский cancel всё равно не будет клобнут.

**HTTP entry:** `POST /content/documents/batch-cancel` (`interfaces/http/content/batch.rs:175`). Принимает массив `document_ids`, в цикле вызывает `cancel_jobs_for_document`. UI-кнопка "Cancel Processing" в `DocumentsOverlays.tsx` доступна при `selectedCount > 0`.

### 12.5. Retry flow (batch reprocess)

Кнопка **"Retry Processing"** на UI работает через `POST /content/documents/batch-reprocess` (`interfaces/http/content/batch.rs:237`). Handler в цикле вызывает `reprocess_single_document`, который:

1. **Force-reset stale inflight** (`services/content/service/document.rs` — новая функция `force_reset_inflight_for_retry`). Это отличает retry от автоматического reconcile: пользовательский retry — явное "останови что бы там ни происходило и начни заново":
   - `cancel_jobs_for_document(document_id)` — отменяет все queued+leased jobs для этого документа.
   - Если `latest_mutation` стоит в `accepted`/`running`, вызывает `reconcile_failed_ingest_mutation` с `failure_code='superseded_by_retry'`, который:
     - Переводит `async_operation` → `failed`
     - Переводит `mutation_items` → `failed`
     - Переводит `mutation` → `failed`
     - Обновляет `content_document_head.latest_mutation_id`
   - Терминальные мутации (`failed`/`canceled`/`applied`) не трогаются.

2. **Admit new mutation** через `admit_mutation(operation_kind='reprocess')`. Новая мутация, новый ingest_job, новая revision, сохраняются те же `content_source_kind`, `storage_key`, `source_uri` через `build_reprocess_revision_metadata` — поэтому web-captured страницы, загруженные файлы и inline-документы retry'ятся одним каноничным путём.

3. **Diff-aware reuse** автоматически пропускает переобработку неизменённых chunk'ов (см. секцию 9). Retry одного и того же контента → SHA256 чексуммы совпадают → `build_chunk_reuse_plan` копирует старые `runtime_graph_extraction` записи без LLM-вызовов. Это делает retry дешёвым по API usage, но гарантирует что всё недоделанное будет пройдено заново (старые attempts уже финализированы, новый воркер начинает с чистого листа).

**Почему это работает для stalled документов (основной use-case):**

Stalled = `queue_state='leased'` + `heartbeat_at` устарел + `mutation_state='accepted'`. Автоматический `reconcile_stale_inflight_mutation_if_terminal` отказывается чинить такое состояние (ждёт `job_state='failed'`), поэтому старый `ensure_document_accepts_new_mutation` бил `ConflictingMutation` и retry молча увеличивал `failed_count` в ответе. Новый `force_reset_inflight_for_retry` принудительно завершает стейл-мутацию, после чего admit идёт штатно.

**Frontend-toast с реальными счётами** (`DocumentsPage.tsx:handleBulkReprocess`): парсит `BatchReprocessResponse { reprocessed_count, failed_count, results }`. Если все ok → success-toast, все failed → error-toast с первой ошибкой, частично → warning-toast `"Запущено X, не удалось Y: {error}"`. Прежняя версия всегда показывала "Reprocessing N documents" независимо от результата.

### 12.6. Матрица resilience

| Сценарий | Покрытие | Как |
|---|---|---|
| Worker процесс рестартнулся | ✅ | Startup sweep при boot pool, порог 30s |
| Backend процесс рестартнулся | ✅ | То же — worker часть backend |
| Провайдер завис >1 мин | ✅ | Periodic reaper, порог 60s |
| Ручной cancel queued job | ✅ | SQL атомарно → `canceled` |
| Ручной cancel leased job | ✅ | SQL → `canceled`, heartbeat observer, pipeline check, finalize canceled |
| Cancel мид-LLM-вызова | ⚠️ | Ждёт завершения текущего стейджа (ограничено провайдерским timeout) |
| Delete document → auto-cancel | ✅ | `cancel_jobs_for_document_with_executor` в транзакции delete |
| Retry stalled документа | ✅ | `force_reset_inflight_for_retry` перед `admit_mutation` |
| Retry web-captured документа | ✅ | Тот же путь, `content_source_kind` сохраняется, diff-aware пропускает неизменные чанки |

---

## 13. Libraries (catalog)

`catalog_library` — Postgres-таблица. Поле `extraction_prompt: Option<String>` — per-library инструкция, которая вставляется в graph extraction prompt как секция `library_context`.

Загружается в `services/content/service/revision.rs:948`:

```rust
let library_extraction_prompt = catalog_repository::get_library_by_id(...)
    .await.ok().flatten()
    .and_then(|row| row.extraction_prompt);
```

Передаётся в `build_canonical_graph_extraction_request(..., library_extraction_prompt)`.

Это место же используется для подгрузки **sub_type hints** (секция 14).

---

## 14. Sub_type flow и vocabulary-aware extraction

### 14.1. Текущее состояние хранения

- `GraphEntityCandidate.sub_type: Option<String>` (`services/graph/extract/types.rs:66`).
- LLM возвращает в JSON, парсится в `parse.rs:223-228`.
- Сохраняется в `runtime_graph_node.metadata_json -> 'sub_type'` через `merge.rs:483-487`.
- В identity не участвует. В retrieval не участвует. Чистая аннотация.

### 14.2. Проблема

`sub_type` намеренно freeform — это решение, чтобы одна и та же graph-модель работала в разных доменах (разработка, медицина, ритейл, право, …), без жёсткого глобального справочника. Минус: LLM при каждой экстракции придумывает значение **с нуля**, не зная, какие `sub_type` уже есть в графе. Результат — для одной сущности появляются варианты `relational_database`, `rdbms`, `relational_db`, которые потом merge'ятся в один узел, но `sub_type` колеблется.

### 14.3. Решение: vocabulary-aware extraction

Вместо отдельной таблицы или materialized view — агрегация налету из `runtime_graph_node.metadata_json`, передача в промпт как **soft hint**.

**Repo-метод:** `infra/repositories/runtime_graph_repository.rs:432` — `list_observed_sub_type_hints(pool, library_id, projection_version) -> Vec<RuntimeGraphSubTypeHintRow>`. Top-N per `node_type` (default 15) применяется уже в caller.

SQL:

```sql
SELECT node_type,
       metadata_json->>'sub_type' AS sub_type,
       COUNT(*) AS occurrences
FROM runtime_graph_node
WHERE library_id = $1
  AND projection_version = $2
  AND metadata_json ? 'sub_type'
  AND length(metadata_json->>'sub_type') > 0
GROUP BY node_type, metadata_json->>'sub_type'
ORDER BY node_type, occurrences DESC, sub_type
```

В коде top-N (по умолчанию 15) per `node_type`.

**Передача в request:** `GraphExtractionRequest.sub_type_hints: GraphExtractionSubTypeHints`. Заполняется helper'ом `load_sub_type_hints_for_extraction` (`services/content/service/revision.rs:1369`), вызываемым в `revision.rs:958` рядом с `library_extraction_prompt`. Тот же call site, тот же скоуп per library, та же projection version (через `resolve_projection_scope`). Один SQL aggregate per revision, результат шарится между chunk'ами через clone в parallel stream (`revision.rs:1063`). Падение SQL/snapshot не валит ingest — функция логирует warn и возвращает `default()`.

**Отображение в промпте:** секция `sub_type_hints` вставляется в `build_graph_extraction_prompt_plan` (`services/graph/extract/prompt.rs:168-170`) **после** `library_context` и **до** `structured_chunk`. Renderer — `render_sub_type_hints` (`prompt.rs:293`). Формат:

```
[sub_type_hints]
Observed sub_types in this library (prefer one of these if it fits;
create a new sub_type only if none match):
- artifact: framework (47), database (32), library (28), microservice (19), …
- attribute: http_status_code (12), latency_ms (8), config_key (6), …
- concept: paradigm (9), pattern (7), …
```

Модель остаётся свободной — это soft hint, не жёсткий enum. Но при наличии якоря 80% обычных кейсов конвергируют естественно.

**Скоуп — per library.** Словари разных доменов не смешиваются, иначе ломается базовая идея freeform per-domain.

**Производительность.** Один SQL aggregate перед стартом extraction для всей revision (не per chunk). Если граф вырастет до объёмов, где это становится узким местом — добавить expression index `(library_id, projection_version, node_type, (metadata_json->>'sub_type'))`. **До этого не оптимизировать.**

### 14.4. Что НЕ делается в этой итерации

- **Embedding snap-to-nearest на write path** — отложено. Появится только если sub_type hints окажутся недостаточны для устранения near-duplicates между сущностями.
- **Offline reconciliation job** — отложено. Для исторического мусора, накопленного до внедрения hints.
- **Сохранение sub_type aliases set per node** — отложено. Текущая политика last-wins при upsert + silent loss при entity resolution не фиксируется в этой итерации; vocabulary-aware extraction на источнике должна снизить частоту коллизий настолько, что post-hoc merge станет редким кейсом.
- **LLM judge на dedup** — не рассматривается.

Если после внедрения hints останутся видимые проблемы — следующий шаг будет именно "правильный merge для sub_type" (alias set + most-frequent-wins) в обоих путях (`upsert_graph_node` + entity resolution merge).

---

## 15. Карта файлов (быстрый индекс)

| Зона | Файлы |
|---|---|
| HTTP entry points | `apps/api/src/interfaces/http/content.rs`, `interfaces/http/ingestion.rs` |
| File parsing | `services/shared/extraction/{file_extract.rs, pdf.rs, docx.rs, spreadsheet.rs, pptx.rs, html_main_content.rs}` |
| Web ingestion | `services/ingest/web.rs`, `services/ingest/web/{single_page.rs, recursive.rs}` |
| Chunking | `services/shared/extraction/chunking.rs` |
| Job runner | `services/ingest/{service.rs, worker.rs, extraction_recovery.rs}` |
| Content service | `services/content/service/{pipeline.rs, revision.rs}` |
| Graph extraction | `services/graph/extract/{types.rs, prompt.rs, parse.rs}` |
| Graph merge | `services/graph/merge.rs`, `services/graph/identity.rs` |
| Entity resolution | `services/graph/entity_resolution.rs` |
| Graph projection | `services/graph/projection.rs` |
| Retrieval | `services/query/{search.rs, execution/retrieve.rs}` |
| Repositories | `infra/repositories/{runtime_graph_repository.rs, catalog_repository.rs}` |
| Schema | `apps/api/migrations/0001_init.sql` (`runtime_graph_node` line 1028) |

---

## 16. UI status taxonomy

Фронт (`apps/web/src/pages/documents/mappers.ts`, `apps/web/src/types/index.ts`) держит две ортогональные дименсии: **readiness** (насколько далеко документ продвинулся к retrieval-готовности) и **status** (что с ним происходит прямо сейчас в pipeline). Badge в списке и инспекторе рендерится по `status`, фильтр-пиллы группируют по `status`, и это единственный источник правды для "что сейчас у этого документа".

### 16.1. `DocumentStatus` enum

```ts
type DocumentStatus =
  | 'queued'         // worker ещё не взял job
  | 'processing'     // worker держит job, heartbeat свежий
  | 'retrying'       // автоматический retry после recoverable сбоя
  | 'blocked'        // waiting на внешнюю зависимость (see `latest_error`)
  | 'stalled'        // worker держит, но heartbeat не тикает — провайдер завис/процесс мёртв
  | 'canceled'       // пользователь отменил через UI
  | 'ready'          // graph готов (или readable без графа)
  | 'ready_no_graph' // graph sparse
  | 'failed';        // терминальная ошибка
```

### 16.2. Derivation chain (`mapApiDocument`)

Строгий приоритет:

1. `queue_state === 'canceled'` → **canceled** (до проверки failed — cancel и реальная ошибка различаются)
2. `readiness === 'failed'` → **failed**
3. `readinessKind === 'graph_ready' | 'readable'` → **ready**
4. `readinessKind === 'graph_sparse'` → **ready_no_graph**
5. `activity_status === 'blocked'` → **blocked**
6. `activity_status === 'retrying'` → **retrying**
7. `activity_status === 'stalled'` → **stalled**
8. `queue_state === 'leased'` → **processing**
9. default → **queued**

Источники сигналов:
- `readinessKind` — `DocumentReadinessSummary.readiness_kind` (из backend).
- `activity_status` — `DocumentReadinessSummary.activity_status`, производится в `services/ingest/activity.rs` из `queue_state` + `heartbeat freshness` + `latest_error`. Это backend-derived поле, фронт его только читает.
- `queue_state`, `claimed_at`, `failure_code` — из `ContentDocumentPipelineJob.latest_job`.

### 16.3. Таймер обработки

```ts
function workerIsHoldingJob(status: DocumentStatus): boolean {
  return status === 'processing' || status === 'stalled' ||
         status === 'blocked'    || status === 'retrying';
}
```

Таймер (`getDocumentProcessingDurationMs`) тикает **только** для статусов, где воркер реально держит job. Source of truth — `claimed_at` (без fallback на `queued_at` или `uploadedAt`). Для `queued`/`canceled`/`ready`/`ready_no_graph`/`failed` таймер не показывается (null → "—" в UI).

**Почему:** старая версия считала время от `queued_at`, и документы в очереди визуально тикали как "обрабатываются 300 секунд" даже когда воркер ещё не брал их в руки. `claimed_at` — это момент, когда attempt был создан (`services/content/service/mod.rs:561` — `map_document_pipeline_job` заполняет `claimed_at = latest_attempt.started_at`).

### 16.4. Filter taxonomy

`documentStatusBucket(status)` в `mappers.ts` группирует все 9 статусов в 4 бакета для фильтр-пиллов:

| Bucket | Statuses | Semantic |
|---|---|---|
| `in_progress` | `queued`, `processing`, `retrying` | Worker двигает документ вперёд (или скоро возьмёт) |
| `attention` | `stalled`, `blocked` | Воркер держит, но ничего не двигается; нужен человек или внешняя зависимость |
| `ready` | `ready`, `ready_no_graph` | Документ готов к retrieval (полностью или частично) |
| `failed` | `failed`, `canceled` | Терминальные состояния, автоматически не продолжатся |

**Бакет `attention` — новый и ключевой для ops.** Если туда попадает документ, это сразу значит "это не движется, иди проверь". Без него stalled/blocked документы сливались с "in progress" и визуально казались обрабатывающимися.

### 16.5. Badge rendering

Единый `buildDocumentStatusBadgeConfig(t)` в `mappers.ts` — source of truth для цвета и лейбла каждого статуса. Используется и в `DocumentsPage.tsx`, и в `DocumentsInspectorPanel.tsx`.

Distinct CSS классы (`index.css`):
- `status-processing` — синий (активная обработка)
- `status-stalled` — красно-оранжевый (новый, визуально отличается от `processing` и `failed`)
- `status-warning` — amber (`blocked`, `retrying`, `ready_no_graph`)
- `status-queued` — нейтральный серый (новый, для `queued` и `canceled`)
- `status-ready` — зелёный
- `status-failed` — красный

Badge показывает `title={doc.statusReason}` как тултип для `stalled`/`blocked`/`failed`, содержит `stalled_reason` из backend'а (например, `"no visible activity for 300s"`).

---

## 17. Известные ограничения

- **OCR не реализован.** Сканированные PDF без текстового слоя выпадают; image OCR заменён на семантическое описание через Vision LLM (другая семантика).
- **Нет специальной обработки GitHub repo / YouTube / API docs** — всё идёт через generic HTML readability.
- **`sub_type` теряется при entity resolution merge** — см. секцию 10. Митигируется vocabulary-aware extraction (секция 14), полный фикс отложен.
- **`relation_type` каталог жёсткий** (`canonical_relation_type_catalog()`), новые типы добавляются только через изменение кода. Это намеренное решение.
- **Entity resolution не использует embedding** — только string-level matching. Может пропускать семантически близкие, но лексически разные сущности.
