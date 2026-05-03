<p align="center">
  <img src="../assets/ironrag-logo.svg" height="64" alt="IronRAG" />
</p>

<h1 align="center">IronRAG</h1>

<p align="center">
  Продакшн-память для AI-агентов и команд.<br/>
  Загрузите документы. Постройте граф знаний. Задайте вопрос. Запускайте агентов.
</p>

<p align="center">
  <img src="../assets/readme-flow.gif" width="720" alt="IronRAG pipeline" />
</p>

---

## Что такое IronRAG?

IronRAG превращает ваши документы, код, PDF, таблицы и веб-страницы в структурированную базу знаний, к которой AI-агенты и люди обращаются мгновенно. Это self-hosted open-source система, которая работает на вашей инфраструктуре и ваши данные остаются под вашим контролем.

В отличие от простых векторных баз, IronRAG строит **граф знаний** из вашего контента: сущности, связи, цепочки подтверждений и ссылки на документы. Агенты, подключенные к IronRAG, не просто ищут текст -- они рассуждают над структурированным знанием.

## Почему IronRAG?

**Для AI-инженеров, строящих продакшн-агентов:**

- **MCP-сервер из коробки.** Подключите Claude, Cursor, VS Code или любого MCP-совместимого агента одной строкой. 21 инструмент: поиск, чтение документов, обход графа, web-ingestion -- всё с разграничением прав по токену.
- **Структурированная память, а не просто эмбеддинги.** Граф знаний фиксирует сущности, типизированные связи и evidence с ранжированием по поддержке. Агенты получают обоснованный контекст, а не шумные similarity-хиты.
- **Мульти-провайдер.** OpenAI, DeepSeek, Qwen или **Ollama для полностью локального вывода** -- без зависимости от облака. Комбинируйте свободно: DeepSeek для рассуждений, OpenAI для эмбеддингов, Ollama для чувствительных данных.
- **CPU-first распознавание документов.** Backend-образ содержит Docling CPU runtime для PDF, document-layout Office-файлов и дефолтного OCR изображений. Таблицы используют native tabular parser. GPU не нужен; OCR raster images можно переключить на активный Vision binding.
- **Учёт стоимости по каждому запросу и документу.** Каждый вызов LLM тарифицируется. Видите стоимость обработки документа и выполнения запроса в дашборде. Можно задать свои тарифы по workspace.

**Для команд, управляющих знаниями:**

- **Загружайте что угодно.** PDF, DOCX, PPTX, XLSX, CSV, Markdown, HTML, код (15 языков с AST-парсингом через tree-sitter), изображения (через vision-модели), веб-страницы (одиночные или рекурсивный обход).
- **Визуализация графа знаний.** Интерактивный WebGL-граф с рендерингом 60fps на 25k+ узлах. Типы сущностей, подтипы, исследование связей, drag, zoom, фильтрация по типам.
- **Обоснованные ответы с источниками.** Каждый ответ ссылается на конкретные разделы документов. Guardrails верификации отклоняют необоснованные утверждения.
- **Полный бэкап и восстановление.** Экспорт в tar.zst архив одним кликом с выбором что включать. Восстановление на том же или другом развёртывании. Спроектирован для GitLab-style бэкап-сценариев.

**Для ops-команд в продакшне:**

- **Гранулярный IAM.** Scoped-токены на уровне системы, workspace или библиотеки. Группы прав контролируют кто может читать, писать, администрировать или подключать агентов.
- **Масштабируется с данными.** Протестировано на библиотеках с 5000+ документами, 25k+ узлами графа, 82k+ связями. Batch-операции, стриминговый экспорт, тюнинг пулов соединений, memory-aware ограничение воркеров.
- **Наблюдаемость.** Prometheus-метрики, structured tracing, аудит-лог с фильтрами по surface/result, тайминги стадий обработки каждого документа.
- **Один Docker Compose.** Postgres, ArangoDB, Redis, backend, worker, frontend -- всё в одном `docker compose up -d`. Helm chart для Kubernetes.

## Как это работает

### Что изменилось с Docling

Пайплайн обработки остался один, но стадия `extract_content` теперь явно
маршрутизирует распознавание по типу файла и recognition policy библиотеки:

- текст, код и таблицы идут через детерминированные `native`-парсеры;
- PDF, DOCX и PPTX идут через встроенный Docling CPU runtime;
- статические raster images по умолчанию идут через Docling OCR;
- OCR raster images можно переключить на активный `vision` binding на уровне библиотеки;
- если выбран `vision`, но binding не настроен, ingest падает явно, без скрытого fallback;
- видеофайлы сейчас не входят в ingest surface.

Новые библиотеки наследуют
`IRONRAG_RECOGNITION_DEFAULT_RASTER_IMAGE_ENGINE=docling`. Для отдельной
библиотеки маршрут меняется через
`PUT /v1/catalog/libraries/{libraryId}/recognition-policy` с
`{"rasterImageEngine":"docling"}` или `{"rasterImageEngine":"vision"}`.

### Пайплайн обработки документа

```mermaid
flowchart TD
  classDef entry fill:#eef6ff,stroke:#3b82f6,stroke-width:2px,color:#0f172a
  classDef api fill:#f8fafc,stroke:#64748b,stroke-width:1.5px,color:#0f172a
  classDef worker fill:#ecfdf5,stroke:#10b981,stroke-width:2px,color:#052e16
  classDef db fill:#fff7ed,stroke:#f97316,stroke-width:2px,color:#431407
  classDef decision fill:#f5f3ff,stroke:#7c3aed,stroke-width:2px,color:#2e1065
  classDef metric fill:#fef9c3,stroke:#eab308,stroke-width:1.5px,color:#422006
  classDef fail fill:#fee2e2,stroke:#ef4444,stroke-width:1.5px,color:#450a0a

  Upload["Загрузка UI / API<br/>файл, метаданные, libraryId"]:::entry
  Admission["Проверка допуска<br/>размер, MIME, расширение, политика<br/>метрики: принято, отклонено"]:::api
  Storage["Хранилище исходника<br/>файловая система или object storage<br/>метрики: bytes, checksum"]:::db
  Revision["Ревизия в Postgres<br/>knowledge_revision=pending<br/>метрика: revision_id"]:::db
  Operation["Асинхронная операция<br/>operation_id, stage=pending"]:::api
  Worker["Runtime воркера<br/>лимиты, повторы и бюджеты<br/>метрика: queue_wait_ms"]:::worker

  Detect{"Тип файла + recognition policy"}:::decision
  Native["native-парсеры<br/>текст, Markdown, HTML, код, таблицы<br/>метрики: parser_ms, chars"]:::worker
  DoclingDocs["Docling CPU-разметка<br/>PDF, DOCX, PPTX<br/>метрики: extract_ms, pages, tables"]:::worker
  RasterPolicy{"Статическое raster image?<br/>rasterImageEngine"}:::decision
  DoclingImage["Docling CPU OCR<br/>PNG/JPG/TIFF/BMP/WEBP по умолчанию<br/>метрики: ocr_ms, chars"]:::worker
  VisionImage["Vision OCR-привязка<br/>облачный или локальный провайдер<br/>метрики: provider, model, cost"]:::worker
  VisionOnly["Маршрут только через Vision<br/>форматы вне Docling-маршрута изображений<br/>требует активный Vision binding"]:::worker
  RecognitionMap["source_map.recognition<br/>engine, capability, structure_tier"]:::metric
  MissingVision["Явная ошибка<br/>Vision binding не настроен<br/>без скрытого fallback"]:::fail
  Unsupported["Явная ошибка<br/>неподдерживаемое видео или binary<br/>нет ingest-ветки"]:::fail

  Normalize["Нормализация и ремонт разметки<br/>очистка технического текста<br/>метрики: normalized_chars, warnings"]:::worker
  Chunk["chunk_content<br/>семантические блоки и окна<br/>метрики: chunk_count, avg_chunk_chars"]:::worker
  Prepare["Подготовка структуры<br/>заголовки, таблицы, block ids<br/>метрика: structured_block_count"]:::worker
  Embed["embed_chunk<br/>привязка провайдера<br/>метрики: embedding_dims, embedded_chunks, cost"]:::worker
  Facts["extract_technical_facts<br/>paths, params, endpoints, config<br/>метрика: fact_count"]:::worker
  Graph["extract_graph<br/>nodes, edges, evidence<br/>метрики: node_count, edge_count"]:::worker
  Finalize["Финализация<br/>revision ready, vector_state=ready<br/>метрика: total_ingest_ms"]:::worker

  Arango["ArangoDB<br/>документы, чанки, векторы,<br/>структурные блоки, факты, граф"]:::db
  Postgres["Postgres<br/>каталог, ревизии,<br/>операции, учёт"]:::db
  Redis["Redis<br/>кеш topology графа<br/>и инвалидация кеша"]:::db
  Projection["Обновление проекции<br/>library projection_version++<br/>метрика: graph_freshness до 10s"]:::metric
  Ready["Документ готов<br/>лексика, векторы, граф,<br/>технические факты"]:::entry

  Upload --> Admission --> Storage --> Revision --> Operation --> Worker --> Detect
  Detect -->|"текст / код / таблицы"| Native
  Detect -->|"PDF / DOCX / PPTX"| DoclingDocs
  Detect -->|"PNG / JPG / TIFF / BMP / WEBP"| RasterPolicy
  RasterPolicy -->|"docling по умолчанию"| DoclingImage
  RasterPolicy -->|"vision"| VisionImage
  Detect -->|"GIF / другое поддерживаемое изображение"| VisionOnly
  Detect -->|"видео / неподдерживаемый binary"| Unsupported
  VisionImage -. binding отсутствует .-> MissingVision
  VisionOnly -. binding отсутствует .-> MissingVision

  Native --> RecognitionMap
  DoclingDocs --> RecognitionMap
  DoclingImage --> RecognitionMap
  VisionImage --> RecognitionMap
  VisionOnly --> RecognitionMap
  RecognitionMap --> Normalize --> Chunk --> Prepare
  Prepare --> Embed
  Prepare --> Facts
  Prepare --> Graph
  Chunk --> Arango
  Embed --> Arango
  Facts --> Arango
  Graph --> Arango
  Extracted["stage_details<br/>recognition + тайминги"]:::metric
  RecognitionMap --> Extracted --> Postgres
  Embed --> Finalize
  Facts --> Finalize
  Graph --> Finalize
  Finalize --> Postgres
  Finalize --> Projection --> Redis --> Ready
  Unsupported --> Postgres
  MissingVision --> Postgres
```

### Пайплайн запроса в базу

```mermaid
flowchart LR
  classDef entry fill:#eef6ff,stroke:#2563eb,stroke-width:2px,color:#0f172a
  classDef runtime fill:#f8fafc,stroke:#64748b,stroke-width:1.5px,color:#0f172a
  classDef retrieve fill:#ecfdf5,stroke:#059669,stroke-width:2px,color:#052e16
  classDef db fill:#fff7ed,stroke:#f97316,stroke-width:2px,color:#431407
  classDef answer fill:#f5f3ff,stroke:#7c3aed,stroke-width:2px,color:#2e1065
  classDef fail fill:#fee2e2,stroke:#dc2626,stroke-width:1.5px,color:#450a0a

  Ask["AI Ассистент / MCP grounded_answer<br/>вопрос, libraryId, session"]:::entry
  Auth["Авторизация и доступ к библиотеке"]:::runtime
  Execution["query_execution<br/>runtimeExecutionId, query_id"]:::runtime
  Rewrite["Контекст диалога<br/>переписывание follow-up, фокус"]:::runtime
  IR["Компилятор запроса IR<br/>intent, scope, target types"]:::runtime

  Arango["ArangoDB<br/>чанки, векторы, факты,<br/>узлы и рёбра графа"]:::db
  Postgres["Postgres<br/>сессии, выполнения,<br/>каталог, трассы"]:::db
  Redis["Redis<br/>кеш IR, кеш графа,<br/>кеш контекста ответа"]:::db

  Vector["Векторная ветка<br/>эмбеддинги чанков"]:::retrieve
  Lexical["Лексическая ветка<br/>BM25, заголовки, literals"]:::retrieve
  Entity["Графовая/entity ветка<br/>сущности и пути evidence"]:::retrieve
  Facts["Ветка технических фактов<br/>paths, params, config keys"]:::retrieve
  Merge["Слияние и дедупликация<br/>чанки + документы + graph evidence"]:::retrieve
  Bundle["Контекстный пакет<br/>citations, prepared refs, graph facts"]:::retrieve

  Route{"Маршрутизатор ответа"}:::answer
  Clarify["Уточнение<br/>тема слишком широкая или есть варианты"]:::answer
  Generate["Генерация grounded answer<br/>выбранный QueryAnswer binding"]:::answer
  Verify["Verifier<br/>strict / moderate / lenient"]:::answer
  Response["Grounded response<br/>answer + citations + verifier"]:::entry
  Fail["Явная ошибка<br/>binding отсутствует или провайдер упал"]:::fail

  Ask --> Auth --> Execution --> Rewrite --> IR
  Execution --> Postgres
  IR <--> Redis
  IR --> Vector
  IR --> Lexical
  IR --> Entity
  IR --> Facts
  Vector <--> Arango
  Lexical <--> Arango
  Entity <--> Arango
  Facts <--> Arango
  Vector --> Merge
  Lexical --> Merge
  Entity --> Merge
  Facts --> Merge
  Merge --> Bundle
  Bundle --> Postgres
  Bundle --> Route
  Route -->|"широко / неоднозначно"| Clarify --> Response
  Route -->|"сфокусированный grounded query"| Generate --> Verify --> Response
  Generate -. ошибка provider .-> Fail
  Verify -. ответ не подтверждён .-> Fail
```

1. **Загрузка** документа (API, UI, MCP или web crawl).
2. **Распознавание** через `native`, Docling CPU или `vision` binding по явной policy.
3. **Нормализация** в structured blocks: заголовки, абзацы, таблицы, код, изображения.
4. **Извлечение** сущностей и связей через LLM -- строится граф знаний.
5. **Эмбеддинг** чанков для векторного поиска.
6. **Запрос** комбинирует vector, lexical, graph/entity и technical-facts lanes.
7. **Ответ** генерируется из собранного контекста и верифицируется по source evidence.

## Технологический стек

| Слой | Технология |
|------|-----------|
| Backend | Rust, Axum, tokio |
| Frontend | React, Vite, TypeScript, Tailwind, shadcn/ui |
| Рендеринг графа | Sigma.js, Graphology (WebGL, Web Worker layout) |
| Хранение документов | PostgreSQL |
| Граф знаний | ArangoDB |
| Координация задач | Redis |
| Парсинг кода | tree-sitter (15 языков) |
| Формат бэкапов | tar.zst (стриминг, чанкованный NDJSON) |

## Быстрый старт

```bash
git clone https://github.com/mlimarenko/IronRAG.git
cd IronRAG/ironrag
cp .env.example .env
# Добавьте ключ: IRONRAG_OPENAI_API_KEY=sk-...
docker compose up -d
```

Откройте [http://127.0.0.1:19000](http://127.0.0.1:19000), создайте admin-аккаунт, загрузите документ и задайте вопрос.

Для полностью локальной работы без облачного провайдера настройте Ollama bindings в Admin-панели.

## Документация

| Тема | Ссылка |
|------|--------|
| Пайплайн обработки | [PIPELINE.md](./PIPELINE.md) |
| MCP-интеграция | [MCP.md](./MCP.md) |
| IAM и токены | [IAM.md](./IAM.md) |
| CLI-справочник | [CLI.md](./CLI.md) |
| Архитектура фронтенда | [FRONTEND.md](./FRONTEND.md) |
| Бенчмарки | [BENCHMARKS.md](./BENCHMARKS.md) |

## Helm-установка

```bash
helm upgrade --install ironrag charts/ironrag \
  --namespace ironrag --create-namespace \
  --set-string app.providerSecrets.openaiApiKey="${OPENAI_API_KEY}" \
  --wait --timeout 20m
```

## Лицензия

[MIT](../../LICENSE)
