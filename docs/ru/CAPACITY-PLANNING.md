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

## Профили хоста

| Профиль | Хост | Форма корпуса | Заметки |
| --- | --- | --- | --- |
| Evaluation | 4 vCPU, 8–12 GiB RAM, 50+ GB disk | Крупнейшая библиотека до ~25k чанков | Подходит для trial и демо. Избегайте крупных rebuild high-dim векторов. |
| Standard | 4–8 vCPU, 16 GiB RAM, 100–150 GB disk | Крупнейшая библиотека до ~250k чанков; суммарный корпус может быть больше за счёт многих библиотек | Соответствует дефолтному memory budget Compose. Нормальный self-hosted режим при редких rebuild. |
| Large | 8–16 vCPU, 24–32 GiB RAM, 250+ GB disk | Крупнейшая библиотека ~250k–1M чанков, высокий ingest concurrency или полные vector rebuild | Поднимите `IRONRAG_*_MEMORY_LIMIT` (см. выше) или эквивалентные overrides в Helm. |

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
раскладка остаётся простой и подходит даже для слабых машин. Чтобы грузить
быстрее (большие импорты, одновременная загрузка многих библиотек), запустите
больше воркеров одной переменной:

```bash
IRONRAG_WORKER_REPLICAS=4 docker compose up -d
```

В Kubernetes задайте `worker.replicaCount` в
[`charts/ironrag/values.yaml`](../../charts/ironrag/values.yaml).

Масштабировать можно в любой момент безопасно: воркеры забирают задачи через
`SELECT … FOR UPDATE SKIP LOCKED` под per-library / per-workspace / глобальными
лимитами конкуренции, поэтому два воркера никогда не возьмут одну задачу и ни
одна библиотека не будет перегружена. У каждого воркера свой бюджет
`IRONRAG_WORKER_MEMORY_LIMIT`, поэтому сайзьте хост под `replicas × память
воркера` поверх БД и backend. Добавление воркеров ускоряет только ingest;
обслуживание запросов масштабируется через backend, а не число воркеров.

См. также: [AI bindings](./AI-BINDINGS.md), [Helm chart](../../charts/ironrag/values.yaml),
[README — деплой](../../README.md#other-deployment-options).
