# Планирование ёмкости

Размер IronRAG определяется **чанками**, плотностью графа, размерностью
embedding и параллелизмом ингеста / пересборки векторного слоя. Число
исходных документов само по себе — слабый предиктор: много небольших
библиотек дешевле в query, чем одна очень большая, а стабильная выдача
ответов требует гораздо меньше RAM, чем native rendering, extract_graph или rebuild
векторного индекса.

Сумма steady-state memory limits в Docker Compose по умолчанию - **5408 MiB**
для базового хоста **8 GiB**. Guard ограничивает её **7208 MiB** (8 GiB минус
12% резерва хоста). Во всех профилях текст изображений распознаёт Vision через
настроенный binding `extract_text`; локальные OCR-модели не устанавливаются.
Native parsing, rendering и кодирование изображений всё ещё требуют ограниченной
RAM. Это настройки сайзинга, а не гарантия ёмкости или качества любого корпуса.
Для более крупных нагрузок поднимайте caps через env (без overlay-файла):

```bash
IRONRAG_DB_MEMORY_LIMIT=6144M \
  IRONRAG_BACKEND_MEMORY_LIMIT=4096M \
  IRONRAG_WORKER_MEMORY_LIMIT=5120M \
  docker compose up -d
```

## Профили хоста

| Профиль | Хост | Форма корпуса | Заметки |
| --- | --- | --- | --- |
| Micro | RAM меньше 8 GiB | Небольшие trial; без крупных vector rebuild | Воркер 768 MiB; сумма caps 3200 MiB. Нужен резерв памяти хоста. |
| Small | От 8 до менее 16 GiB RAM | Evaluation workloads | Воркер 1536 MiB; сумма caps 5952 MiB. |
| Medium | От 16 до менее 32 GiB RAM | Стандартные нагрузки; проверьте целевой корпус | Воркер 3072 MiB; сумма caps 7488 MiB. |
| Large | От 32 GiB RAM | Крупные графы или vector rebuild; проверьте целевой корпус | Воркер 5120 MiB; сумма caps 21504 MiB. |

Это рекомендации установщика, а не гарантия размера корпуса. Установщик оставляет
хосту max(512 MiB, 12% RAM) и сохраняет существующие caps без
`--recompute-resources`. Для изображений и сканов настройте Vision-совместимый
binding `extract_text` независимо от профиля.

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

Пересборка векторного индекса может вызывать крупные пики памяти базы. При смене embedding binding
на другую размерность или rebuild high-dimensional shard проводите работы в
maintenance window и поднимайте memory-лимиты для million-row shard'ов.

## Память при пересборке графа и диагностика OOM

Пересборка графа библиотеки читает extraction records через PostgreSQL cursor
внутри своей транзакции пакетами до 64 записей.
Весь raw и normalized extraction JSON библиотеки не собирается в памяти сразу.
Проверка и очистка графа используют компактные идентичности вместо второй копии
всех payloads узлов и рёбер. При этом остаются индексы полноты документов и
идентичностей графа: расход памяти не постоянен и зависит от payload пакета
и графа, а также от параллельных заданий worker.

Ограниченное чтение не ограничивает число обрабатываемых записей и не разрешает
частичную публикацию. Проверки полноты и публикация графа остаются
транзакционными: при ошибке ранее опубликованный граф не меняется.
Прогрев топологии после commit выделяет память отдельно; его тоже нужно
включать в проверку.

Свободная RAM хоста не отменяет жёсткий лимит контейнера. При OOM сопоставьте
kernel evidence `CONSTRAINT_MEMCG` или host-wide OOM с лимитом контейнера,
статусом `OOMKilled`, историей рестартов, RSS и working set. Счётчик OOM kills
на узле учитывает и cgroup-triggered завершения; сам по себе он не доказывает
исчерпание RAM всего хоста. Интервал scrape может пропустить короткий пик
выделения памяти, поэтому низкий предыдущий sample не противоречит RSS жертвы
в kernel log. До вывода об утечке или повышения лимитов отделите рост
удерживаемого baseline от кратковременного пика.

## Лимиты filtered HNSW-поиска

Vector relations общие для одной размерности; library, embedding profile и
vector kind являются логическими фильтрами внутри relation. Поэтому pgvector
работает со strict iterative scan и явными границами:

```text
IRONRAG_PG_HNSW_BUILD_BUDGET_BYTES=3000000000
IRONRAG_PG_HNSW_EF_SEARCH=400
IRONRAG_PG_HNSW_MAX_SCAN_TUPLES=50000
IRONRAG_PG_HNSW_SCAN_MEM_MULTIPLIER=2
IRONRAG_PG_HNSW_EXACT_FALLBACK_MAX_ROWS=10000
```

Runtime ограничивает значения диапазонами `1..10000`, `1..1000000`, `1..64`
и `0..100000`. Если approximate scan вернул меньше строк, чем запрошено, хотя
manifest логического lane содержит достаточно строк, IronRAG пишет
структурированный warning. Для lane не крупнее exact-fallback threshold
выполняется один точный retry; `0` отключает его. Крупные lane остаются
ограниченными и только сообщают об underfill, не запуская потенциально
неограниченную сортировку расстояний.

Поднимайте scan tuples или memory multiplier только после сравнения качества
ответов, query p95 и памяти PostgreSQL. Memory multiplier масштабирует
использование `work_mem` в pgvector, поэтому большое значение при параллельных
запросах может заметно увеличить расход памяти БД.

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

На gateway провайдера действует второй уровень backpressure, отдельный для
каждого endpoint:

```text
IRONRAG_PROVIDER_CONCURRENCY_MAX_OUTBOUND=16
IRONRAG_PROVIDER_CONCURRENCY_QUERY_RESERVED=4
IRONRAG_PROVIDER_CONCURRENCY_ACQUIRE_TIMEOUT_MS=30000
IRONRAG_PROVIDER_RATE_LIMIT_REQUESTS_PER_SECOND=8
IRONRAG_PROVIDER_RATE_LIMIT_BURST=16
IRONRAG_PROVIDER_RETRY_MAX_ATTEMPTS=3
IRONRAG_PROVIDER_RETRY_INITIAL_BACKOFF_MS=200
IRONRAG_PROVIDER_RETRY_MAX_BACKOFF_MS=5000
```

Ingest использует только 12 общих permit-ов и не может вытеснить интерактивные
запросы. Для каждого endpoint также действует отдельный token bucket. Retry
использует ограниченный exponential backoff с jitter и учитывает корректный
`Retry-After` до настроенного максимума. Ожидание permit и token ограничено и
завершается fail-closed ошибкой. `0`/`0` отключает соответствующий бюджет;
reserve, равный максимуму concurrency, отклоняется при старте, поскольку он
заблокировал бы ingest.

Бюджет действует **на процесс**, а не глобально. Каждая реплика API, каждый
worker и каждый запуск `ironrag-maintenance` или `ironrag-cli` держат свой
собственный реестр в памяти. Реальная нагрузка на провайдера равна
`число процессов × IRONRAG_PROVIDER_RATE_LIMIT_REQUESTS_PER_SECOND`; сверять
с квотой провайдера нужно именно это произведение, а не однопроцессное число.

`IRONRAG_PROVIDER_CONCURRENCY_MAX_OUTBOUND=0` не исключает endpoint из
реестра: rate limiter включён по умолчанию, и ему нужна собственная запись.
Такие endpoint-ы учитываются в
`IRONRAG_PROVIDER_CONCURRENCY_REGISTRY_MAX_ENTRIES`, который при переполнении
завершается fail-closed. Чтобы endpoint остался полностью без учёта, задайте
`0` и для concurrency-максимума, и для requests-per-second.

Каждый worker дополнительно применяет локальный claim cap, рассчитанный из его
cgroup soft memory limit, до того как забирает новые ingest jobs из
Postgres. Это не даёт memory-heavy extraction или graph-merge задачам
накопиться в одном процессе на swapless host. Поднимать
`IRONRAG_WORKER_REPLICAS` или ingest caps имеет смысл только вместе с host
capacity, worker memory limit, DB connection budget и provider concurrency
budget.

У каждого воркера свой бюджет `IRONRAG_WORKER_MEMORY_LIMIT`, поэтому сайзьте
хост под `replicas × память воркера` поверх БД и backend.
Native extraction сохраняет page batching, timeout 300 секунд и ограничения
допуска процессов. Изображения распознаёт удалённый Vision. Helm по умолчанию
запускает один воркер с request 768 MiB и limit 3 GiB; Compose задаёт 3072 MiB.
Перед увеличением concurrency проверьте scheduler reservations и пиковый RSS
на представительных документах. Снижение reservation не означает экономию idle RSS.
`IRONRAG_DATABASE_MAX_CONNECTIONS` — общий app-бюджет соединений на deployment:
runtime делит его между ожидаемыми API и worker процессами и ограничивает
локальный claim-loop worker-а тем числом DB слотов, которое он может обслужить.
Добавление воркеров увеличивает DB-backed ingest parallelism только если общий
DB-бюджет имеет запас под большее число процессов. С дефолтным бюджетом
дополнительные воркеры в основном распределяют работу и memory isolation между
процессами; обслуживание запросов масштабируется через backend, а не число
воркеров.

Потолок 16 соединений на процесс снят: `IRONRAG_DATABASE_MAX_CONNECTIONS`
делится на API+worker replica, и это частное есть process budget.
Внешние retrieval-turn проходят через admission semaphore
(`IRONRAG_QUERY_TURN_MAX_CONCURRENT` по умолчанию 32, очередь 80, ожидание 15с).
Всплеск сверх очереди отвергается retryable HTTP 503
`query_admission_rejected`, а не ждёт 180с turn budget и не исчерпывает SQLx
pool. Matching-terms retrieval использует expression-индексы `pg_trgm` на
`lower(normalized_text)`, `lower(content_text)` и
`lower(coalesce(window_text, ''))`. Кандидаты собираются `UNION`-ом
поколоночных `LIKE` от bound-паттернов `'%term%'`; не складывать три
`LIKE` через `OR` и не джойнить requested-ревизии первым — такой план
игнорирует `gin_trgm`. Не возвращать prefilter
`to_tsquery('simple', prefix:*)` — он пропускает substring внутри лексем
URL/`key=value`.

Дефолты bundled Postgres остаются консервативными для контейнера 1 GiB:
`max_connections=100`, `shared_buffers=128MB`, `work_mem=4MB`. Сначала
поднимайте memory limit контейнера, потом `shared_buffers` / `work_mem`.

### Как масштабировать память Postgres под своё железо

Три compose-переменные масштабируются вместе от лимита контейнера базы
(`IRONRAG_DB_MEMORY_LIMIT`):

| Лимит контейнера БД | `IRONRAG_POSTGRES_SHARED_BUFFERS` | `IRONRAG_POSTGRES_WORK_MEM` | `IRONRAG_POSTGRES_SHM_SIZE` |
| --- | --- | --- | --- |
| 1 GiB (дефолт) | 128MB | 4MB | 256mb |
| 4 GiB | 1GB | 8MB | 1g |
| 6 GiB | 3GB | 12MB | 2g |
| 12 GiB | 6GB | 16MB | 2g |
| 24 GiB+ | 8–12GB | 32MB | 4g |

Правила, стоящие за таблицей:

- **`shared_buffers` ≈ 40–50 % лимита контейнера.** Остаток cgroup занимают
  приватная память бэкендов и страничный кэш ядра для чтений мимо буферного
  пула. Страничный кэш тоже учитывается в cgroup контейнера; возможность его
  освобождения не гарантирует резерв для параллельных приватных выделений.
- **`work_mem` тратится на каждый узел сортировки/хэша в каждом бэкенде** и
  потому умножается на конкурентность, в отличие от `shared_buffers`. При
  `max_connections=300` значение 32MB уводит худший случай далеко за контейнер
  в 6 GiB; сначала растите `shared_buffers`.
- **`shm_size` задаёт размер `/dev/shm`** — там живёт динамическая общая
  память параллельных запросов; её спрос следует за `work_mem` × число
  параллельных воркеров, а не за `shared_buffers`. 2 GiB хватает с запасом;
  поднимайте при ошибках `could not resize shared memory segment`.
- Что бы вы ни меняли, проверяйте фактические значения изнутри сервера — эти
  настройки приходят в командной строке postmaster, поэтому `ALTER SYSTEM` их
  не меняет, а `pg_settings.source` показывает `command line`:

  ```sql
  select name, setting, source from pg_settings
  where name in ('shared_buffers', 'work_mem', 'max_connections');
  ```

Цель всего этого — попадание в кэш (запрос выше): на стенде, чей корпус
многократно превышал память, подъём `shared_buffers` с 1 GB до 3 GB вместе с
настройками асинхронного ввода-вывода ниже поднял бенчмарк на 100 одновременных
ходов с 34 до 52 завершений при тех же лимитах допуска. При высокой
конкурентности выбирайте больший `shared_buffers`, а не `work_mem`.

### Под конкурентной нагрузкой упор в диск, а не в процессор

Когда корпус перестаёт помещаться в `shared_buffers`, латентность определяют
физические чтения, и симптом обманчив: **приложение выглядит простаивающим**.
На стенде при 100 одновременных ходах бэкенд занимал 0.18 ядра из 8, а
Postgres накопил ~2300 с времени операторов — он ждал диск, а не считал.
Загрузка процессора не отличает «ждём сеть» от «ждём чтение»; прежде чем
объявить нагрузку упирающейся в провайдера, сравните накопленное время
операторов с временем вызовов провайдера.

Сначала проверьте попадание в кэш:

```sql
select round(100.0 * sum(heap_blks_hit)
             / nullif(sum(heap_blks_hit + heap_blks_read), 0), 1) as hit_pct
from pg_statio_user_tables;
```

Меньше ~95% означает, что рабочий набор не помещается и каждый запрос платит
задержкой диска.

### Асинхронный ввод-вывод PostgreSQL 18

PostgreSQL 18 читает последовательные сканы, **bitmap heap scan** и vacuum
асинхронно. Триграммные и полнотекстовые дорожки IronRAG — это как раз bitmap
heap scan, а поставляемые дефолты (`io_method=worker`, `io_workers=3`,
`effective_io_concurrency=16`) рассчитаны на небольшой хост.

| Параметр | Дефолт | Для узла, упирающегося в диск | Почему |
| --- | --- | --- | --- |
| `io_workers` | 3 | 8 | Три воркера не держат очередь чтений заполненной под конкурентной нагрузкой. |
| `effective_io_concurrency` | 16 | 256 (SSD/NVMe) | Глубина упреждающего чтения для bitmap heap scan. |
| `maintenance_io_concurrency` | 16 | 64 | То же для vacuum и построения индексов. |
| `random_page_cost` | 4.0 | 1.1 (SSD/NVMe) | Значение 4.0 кодирует время позиционирования **вращающегося** диска и уводит планировщик от индексных сканов. Сначала проверьте устройство: `lsblk -d -o NAME,ROTA` (`ROTA=0` — твердотельный). |
| `track_io_timing` | off | on | Без него `pg_stat_io` и `EXPLAIN (BUFFERS)` не покажут время чтения. |

`io_method=io_uring` отличается от `worker`, но **seccomp-профиль Docker
по умолчанию блокирует системные вызовы io_uring**: Postgres не стартует с
`could not setup io_uring queue: Operation not permitted`. Для него нужен
собственный seccomp-профиль — это осознанное решение по безопасности, тогда
как `worker` с увеличенным `io_workers` исключений не требует.

Откат неудачного `io_method` неудобен: сервер не поднимется, чтобы принять
отменяющий `ALTER SYSTEM`. Правьте `postgresql.auto.conf` в томе данных при
остановленном контейнере.

### Индексы по выражению теряют статистику при REINDEX

Индекс, построенный на выражении, несёт собственную статистику, и планировщику
больше нечем оценить селективность этого выражения. `REINDEX INDEX
CONCURRENTLY` строит **новый** индекс и подменяет им старый, поэтому статистика
пропадает до ближайшего `ANALYZE` — автовакуум может прийти через часы, и всё
это время сбой ничем себя не проявляет.

Замер на стенде: `LIKE ANY(...)` по триграммному индексу над конкатенацией
текстовых колонок без статистики выражения — планировщик оценил **140 строк**
там, где скан вернул **47 724**, промах в 341 раз, распространяющийся на все
соединения выше. Обычный `ANALYZE` по таблице привёл оценку к 10 676 — всё ещё
занижено, но в пределах, с которыми планировщик работает.

```sql
-- индексы по выражению без статистики
select t.relname as table, c.relname as index
from pg_class c
join pg_index i on i.indexrelid = c.oid
join pg_class t on t.oid = i.indrelid
where i.indexprs is not null
  and not exists (select 1 from pg_stats s where s.tablename = c.relname);
```

После `REINDEX` индекса по выражению всегда выполняйте `ANALYZE` его таблицы.

`reltuples` у GIN-индекса при этом не является признаком здоровья: значения,
многократно превышающие число строк таблицы, нормальны для индексов над
большими конкатенированными выражениями, а переиндексация может это число
увеличить, а не уменьшить. Судите о таких индексах по оценкам строк в плане, а
не по `reltuples`.

## Варианты Compose-деплоя

Все варианты живут в одном `docker-compose.yml` и выбираются env + одним
profile (полный список — в заголовке файла):

```bash
# S3-совместимое хранилище (bundled s4core), profile `s4`
COMPOSE_PROFILES=s4 \
  IRONRAG_CONTENT_STORAGE_PROVIDER=s3 \
  IRONRAG_DEPENDENCY_OBJECT_STORAGE_MODE=bundled \
  IRONRAG_CONTENT_STORAGE_TOPOLOGY=shared_cluster \
  docker compose up -d

# Локальная сборка. IRONRAG_VERSION — идентификатор сборки,
# который зашивается в образ и отдаётся по /v1/version; без него сборка падает,
# а не подставляет заглушку. `make docker-local-build` выводит его из git сам.
IRONRAG_VERSION=$(git describe --always --abbrev=40 --dirty --match '') \
  IRONRAG_BACKEND_IMAGE=ironrag-backend:local \
  IRONRAG_FRONTEND_IMAGE=ironrag-frontend:local \
  docker compose build backend frontend
IRONRAG_BACKEND_IMAGE=ironrag-backend:local \
  IRONRAG_FRONTEND_IMAGE=ironrag-frontend:local \
  docker compose up -d
```

### Helm (Kubernetes)

```bash
helm upgrade --install ironrag charts/ironrag \
  --namespace ironrag --create-namespace \
  --set-string app.providerSecrets.provider_alpha="${PROVIDER_API_KEY}" \
  --wait --timeout 20m
```

По умолчанию chart деплоит API, worker и web с тегом `v<appVersion>` из
`Chart.appVersion`. Переопределяйте `api.image.tag`, `worker.image.tag` и
`web.image.tag` только при pin на другой published image. Bundled
зависимости совпадают с Docker Compose: `pgvector/pgvector:pg18` и
`redis:8.10.1`.

См. также: [AI bindings](./AI-BINDINGS.md), [Установка](./INSTALL.md),
[Helm chart](../../charts/ironrag/values.yaml).
