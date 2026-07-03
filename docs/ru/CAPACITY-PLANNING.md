# Планирование ёмкости

Размер IronRAG определяется **чанками**, плотностью графа, размерностью
embedding и параллелизмом ингеста / пересборки векторного слоя. Число
исходных документов само по себе — слабый предиктор: много небольших
библиотек дешевле в query, чем одна очень большая, а стабильная выдача
ответов требует гораздо меньше RAM, чем OCR, extract_graph или rebuild
векторного индекса.

Для trial можно брать меньший хост, но дефолтный стек Docker Compose
рассчитан на **16 GiB**. На более крупных машинах поднимайте per-role
memory-лимиты через env (без отдельного overlay-файла):

```bash
IRONRAG_DB_MEMORY_LIMIT=6144M \
  IRONRAG_BACKEND_MEMORY_LIMIT=4096M \
  IRONRAG_WORKER_MEMORY_LIMIT=4096M \
  docker compose up -d
```

## DNS в контейнерах

Compose-стек задаёт app-контейнерам явные recursive DNS defaults, чтобы
внешние provider endpoints стабильно резолвились даже если Docker daemon
унаследовал host resolver, недоступный из контейнеров. Переопределяйте их,
когда окружение должно ходить через приватный resolver:

```bash
IRONRAG_DOCKER_DNS_PRIMARY=192.0.2.53 \
  IRONRAG_DOCKER_DNS_SECONDARY=192.0.2.54 \
  docker compose up -d
```

## Профили хоста

| Профиль | Хост | Форма корпуса | Заметки |
| --- | --- | --- | --- |
| Evaluation | 4 vCPU, 8–12 GiB RAM, 50+ GB disk | Крупнейшая библиотека до ~25k чанков | Подходит для trial и демо. Избегайте крупных rebuild high-dim векторов. |
| Standard | 4–8 vCPU, 16 GiB RAM, 100–150 GB disk | Крупнейшая библиотека до ~250k чанков; суммарный корпус может быть больше за счёт многих библиотек | Соответствует дефолтному memory budget Compose. Нормальный self-hosted режим при редких rebuild. |
| Large | 8–16 vCPU, 24–32 GiB RAM, 250+ GB disk | Крупнейшая библиотека ~250k–1M чанков, высокий ingest concurrency или полные vector rebuild | Поднимите `IRONRAG_*_MEMORY_LIMIT` (см. выше) или эквивалентные overrides в Helm. |

## Runtime graph prewarm

Prewarm runtime graph projection по умолчанию выключен:

```text
IRONRAG_RUNTIME_GRAPH_PROJECTION_PREWARM_ENABLED=false
IRONRAG_RUNTIME_GRAPH_PROJECTION_PREWARM_MAX_LIBRARIES=0
```

На ограниченных или multi-library хостах оставляйте его выключенным. Lazy
per-library загрузка графа сохраняет query service доступным без загрузки
каждого активного library graph при старте API. Включайте prewarm только когда
на хосте достаточно свободной RAM для крупнейших активных graph projections, а
latency первого graph turn уже стала операционной проблемой. При включении
используйте `IRONRAG_RUNTIME_GRAPH_PROJECTION_PREWARM_MAX_LIBRARIES`, чтобы
ограничить startup memory exposure до подъёма API memory limits.

## Диск

```text
database disk ~= chunks * (10–20 KB content+graph)
              + vector rows * embedding_dim * component_size * index_factor
              + original file storage
              + 20–30% headroom
```

`component_size` — `4` байта для `vector(dim)` при `dim <= 2000` и `2` байта
для `halfvec(dim)` при `dim > 2000`. Практически 1536-dim `vector` и
3072-dim `halfvec` дают ~6 KB сырого vector payload на строку. HNSW и SQL-индексы
добавляют overhead — для планирования берите `index_factor = 2–3`.

| Размер embedding | Сырой vector payload на 100k строк | На 1M строк |
| --- | ---: | ---: |
| 384-dim `vector` | ~150 MB | ~1.5 GB |
| 1536-dim `vector` | ~600 MB | ~6 GB |
| 3072-dim `halfvec` | ~600 MB | ~6 GB |

Например, корпус на миллион чанков с 3072-dim embeddings — около 6 GB сырого
chunk-vector payload до индексов, строк графа, исходных файлов, WAL и backup.
Multi-library корпус на несколько миллионов чанков может уложиться в standard
RAM-профиль, если крупнейшая активная библиотека умеренная и rebuild
запланированы; диск растёт с суммарным корпусом.

## Пики при vector rebuild

Главный memory spike — пересборка векторного индекса. При смене embedding binding
на другую размерность или rebuild high-dimensional shard проводите работы в
maintenance window и поднимайте memory-лимиты для million-row shard'ов.

## Масштабирование ingest-воркеров

Ingest — извлечение, чанкинг, эмбеддинг и сборка графа — работает в отдельном
сервисе **worker**. По умолчанию стек запускает **один** воркер: базовая
раскладка остаётся простой и подходит даже для слабых машин. Чтобы распределить
ingest независимее (большие импорты, одновременная загрузка многих библиотек),
запустите больше воркеров одной переменной:

```bash
IRONRAG_WORKER_REPLICAS=4 docker compose up -d
```

В Kubernetes задайте `worker.replicaCount` в
[`charts/ironrag/values.yaml`](../../charts/ironrag/values.yaml). Helm chart
также резервирует один API rollout surge slot в DB-бюджете, чтобы rolling update
API оставался доступным и не открывал неожиданные Postgres backend'ы.

Если API масштабируется вне Helm, задайте `IRONRAG_API_REPLICAS` равным числу
API-процессов, которые могут быть живы одновременно. Для rolling update
учитывайте surge-процесс. Compose по умолчанию считает один API и один worker;
Helm выводит API count как `api.replicaCount + 1`, а worker count как
`worker.replicaCount`.

Масштабировать можно в любой момент безопасно: воркеры забирают задачи через
`SELECT … FOR UPDATE SKIP LOCKED` под per-library / per-workspace / глобальными
лимитами конкуренции, поэтому два воркера никогда не возьмут одну задачу и ни
одна библиотека не будет перегружена. Дефолтные лимиты намеренно консервативны:

```text
IRONRAG_INGESTION_MAX_PARALLEL_JOBS_GLOBAL=4
IRONRAG_INGESTION_MAX_PARALLEL_JOBS_PER_WORKSPACE=2
IRONRAG_INGESTION_MAX_PARALLEL_JOBS_PER_LIBRARY=1
```

Каждый worker дополнительно применяет локальный claim cap, рассчитанный из его
cgroup soft memory limit, до того как забирает новые canonical ingest jobs из
Postgres. Это не даёт memory-heavy extraction или graph-merge задачам
накопиться в одном процессе на swapless host. Поднимать
`IRONRAG_WORKER_REPLICAS` или ingest caps имеет смысл только вместе с host
capacity, worker memory limit, DB connection budget и provider concurrency
budget.

У каждого воркера свой бюджет `IRONRAG_WORKER_MEMORY_LIMIT`, поэтому сайзьте
хост под `replicas × память воркера` поверх БД и backend.
Docling-backed PDF, office и OCR extraction также требуют достаточный hard
cgroup headroom для одного локального extractor process. Если worker cap
слишком мал, ingest завершает затронутый документ ошибкой
`docling_insufficient_memory` до запуска extractor; для extractor-heavy
импортов поднимайте `IRONRAG_WORKER_MEMORY_LIMIT`, а не рассчитывайте на retry.
`IRONRAG_DATABASE_MAX_CONNECTIONS` — общий app-бюджет соединений на deployment:
runtime делит его между ожидаемыми API и worker процессами и ограничивает
локальный claim-loop worker-а тем числом DB слотов, которое он может обслужить.
Добавление воркеров увеличивает DB-backed ingest parallelism только если общий
DB-бюджет имеет запас под большее число процессов. С дефолтным бюджетом
дополнительные воркеры в основном распределяют работу и memory isolation между
процессами; обслуживание запросов масштабируется через backend, а не число
воркеров.

См. также: [AI bindings](./AI-BINDINGS.md), [Helm chart](../../charts/ironrag/values.yaml),
[README — деплой](../../README.md#other-deployment-options).
