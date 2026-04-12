<p align="center">
  <img src="./docs/assets/ironrag-logo.svg" alt="IronRAG logo" width="180">
</p>

# IronRAG

### Система знаний для документов, внутренних ботов и AI-агентов

[README](./README.md) | [MCP](./docs/ru/MCP.md) | [CLI](./docs/ru/CLI.md) | [IAM](./docs/ru/IAM.md)

> Один `docker compose up` — веб-интерфейс, MCP endpoint и канонический пайплайн для внутренних ассистентов, саппорта и агентных сценариев.

## Архитектура

Снаружи один порт на **web**. Фронтенд-контейнер отдает SPA (**React + Vite**) и проксирует `/v1/*` в **Rust/Axum** API. Тот же backend-образ используется как **worker** и как отдельная one-shot роль **startup**, которая выполняет миграции, Arango bootstrap и инициализацию storage. `s4core` опционален и используется только в S3 compose/Helm профилях.

```text
                         ┌─────────────────────────┐
                         │    web (SPA + /v1)      │
                         └───────────┬─────────────┘
               ┌─────────────────────┴─────────────────────┐
        GET /* (SPA)                                 /v1/* (API + MCP)
      ┌────────▼─────────┐                       ┌─────────▼──────────┐
      │    frontend      │                       │      backend       │
      │  React + Vite    │                       │   Rust / Axum      │
      └──────────────────┘                       └─────────┬──────────┘
                         ┌─────────────────────────────────┼────────────────────┐
                  ┌──────▼──────┐                  ┌───────▼───────┐    ┌───────▼───────┐
                  │  ArangoDB   │                  │   Postgres    │    │    Redis      │
                  │ graph+vector│                  │ IAM + control │    │ worker queue  │
                  └─────────────┘                  └───────────────┘    └───────┬───────┘
                                           ┌────────────────────────────┴───────┬───────────────┐
                                           │                                    │               │
                                     ┌─────▼─────┐                        ┌─────▼─────┐   ┌────▼────┐
                                     │  startup  │                        │  worker   │   │ s4core  │
                                     └───────────┘                        └───────────┘   └─────────┘
```

## Пайплайн

```text
загрузка / URL → извлечение текста → структурные блоки → фильтр boilerplate
  → семантический чанкинг (2800 символов, 10% overlap, heading-aware)
  → эмбеддинги → графовое извлечение (v6: 10 типов сущностей, 88 типов связей)
  → резолюция сущностей (alias/acronym merge) → суммаризация документа
  → scoring качества → гибридный индекс (BM25 + vector) → UI + MCP + API
```

## Деплой

Нужен Docker с Compose v2 или Kubernetes с Helm.

```bash
# Установка без клона репозитория
curl -fsSL https://raw.githubusercontent.com/mlimarenko/IronRAG/master/install.sh | bash

# Или из клонированного репозитория
cp .env.example .env
docker compose up -d
```

Compose-профили:

- `docker-compose.yml` — bundled Postgres/Redis/ArangoDB + filesystem storage
- `docker-compose-s4.yml` — bundled Postgres/Redis/ArangoDB + bundled `s4core` + S3 storage
- `docker-compose-local.yml` — локальная сборка из исходников

Примеры:

```bash
docker compose up -d
docker compose -f docker-compose-s4.yml up -d
docker compose -f docker-compose-local.yml up --build -d
```

UI по умолчанию: [http://127.0.0.1:19000](http://127.0.0.1:19000)

Helm:

```bash
OPENAI_API_KEY=... \
helm upgrade --install ironrag charts/ironrag \
  --namespace ironrag \
  --create-namespace \
  --values charts/ironrag/values/examples/bundled-s3.yaml \
  --set-string app.frontendOrigin=https://ironrag.example.com \
  --set-string app.providerSecrets.openaiApiKey="${OPENAI_API_KEY}" \
  --wait \
  --wait-for-jobs \
  --timeout 20m
```

Внешние зависимости:

```bash
helm upgrade --install ironrag charts/ironrag \
  --namespace ironrag \
  --create-namespace \
  --values charts/ironrag/values/examples/external-services.yaml
```

Профили chart:

- `bundled-s3.yaml` — bundled Postgres/Redis/ArangoDB + bundled `s4core`
- `external-services.yaml` — внешние Postgres/Redis/ArangoDB/S3
- `filesystem-single-node.yaml` — только single-node режим с filesystem

Minikube используется только для локальной проверки chart, а не как профиль деплоя.

Полный справочник runtime-конфигурации: [apps/api/.env.example](./apps/api/.env.example)

## Возможности

- **Ingestion** — текст, код (50+ расширений), PDF, DOCX, PPTX, HTML, изображения, веб-страницы
- **Типизированный граф знаний** — 10 универсальных типов сущностей (person, organization, location, event, artifact, natural, process, concept, attribute, entity), 88 типов связей, entity resolution
- **Гибридный поиск** — BM25 + vector cosine через Reciprocal Rank Fusion, field-weighted scoring (заголовки 1.5x)
- **Grounded-ассистент** — встроенный чат с верификацией ответов и evidence panel
- **21 MCP-инструмент** — поиск, чтение, загрузка, граф, веб-краулинг, администрирование (встроенный ассистент использует те же тулы, как обычный MCP-клиент)
- **Умный чанкинг** — 2800 символов, 10% overlap, heading-aware, code-aware, boilerplate detection, quality scoring
- **Доступ и контроль** — API-токены с 13 видами прав, иерархические скоупы (system/workspace/library/document), фильтрация MCP-инструментов по грантам
- **Admin CLI** — `ironrag-cli` для управления пользователями/токенами, workspace/библиотеками, экспорта/импорта без HTTP
- **Учёт затрат** — стоимость обработки по документу и библиотеке
- **Выбор моделей** — разные провайдеры и модели для каждого этапа пайплайна

## MCP для агентов

21 инструмент из коробки. Токен → гранты → сниппет из `Admin -> MCP`.

| Категория | Инструменты |
|-----------|-------------|
| **Документы** | `search_documents`, `read_document`, `list_documents`, `upload_documents`, `update_document`, `delete_document` |
| **Граф** | `search_entities`, `get_graph_topology`, `list_relations` |
| **Веб-краулинг** | `submit_web_ingest_run`, `get_web_ingest_run`, `cancel_web_ingest_run` |
| **Обнаружение** | `list_workspaces`, `list_libraries` |

Ответы search/read по умолчанию используют `includeReferences=false` для минимизации токенов. Подробнее: [MCP](./docs/ru/MCP.md) | [IAM и токены](./docs/ru/IAM.md) | [CLI](./docs/ru/CLI.md)

## Стек

| Слой | Технологии |
|------|-----------|
| API + worker | Rust, Axum, SQLx |
| Фронтенд | React, Vite, Tailwind, shadcn/ui |
| Граф и векторы | ArangoDB 3.12 |
| Control plane | PostgreSQL 18 |
| Очередь | Redis 8 |
| Edge / SPA | nginx 1.28 внутри `web` |
| Деплой | Helm, Docker Compose, Ansible |

## Конфигурация

Все переменные — `IRONRAG_*`. Ключевые файлы:

| Файл | Назначение |
|------|-----------|
| `.env.example` | Compose-переменные |
| `apps/api/.env.example` | Полный справочник runtime-конфигурации |
| `apps/api/src/app/config.rs` | Встроенные дефолты |

## Бенчмарки

В репозитории два golden dataset: Wikipedia corpus (30 вопросов) и code corpus (20 вопросов, 8 файлов на Go/TS/Python/Rust/Terraform/React/K8s/Docker).

```bash
export IRONRAG_SESSION_COOKIE="..."
export IRONRAG_BENCHMARK_WORKSPACE_ID="workspace-uuid"
make benchmark-grounded-seed   # загрузить corpus
make benchmark-grounded-all    # прогнать QA matrix
make benchmark-golden          # golden dataset
```

Подробнее: [apps/api/benchmarks/grounded_query/README.md](./apps/api/benchmarks/grounded_query/README.md).

## Дорожная карта

- [x] Гибридный поиск (BM25 + vector RRF fusion)
- [x] Графовое извлечение v6 (few-shot, 10 типов сущностей, 88 связей)
- [x] Семантический чанкинг (2800 chars, overlap, heading-aware, code-aware)
- [x] Boilerplate detection, quality scoring, entity resolution
- [x] 21 MCP-инструмент: поиск, чтение, загрузка, навигация по графу
- [x] Типизированная раскраска и подписи рёбер в UI графа
- [x] Параллельное извлечение (до 8 конкурентных чанков)
- [x] SSE-стриминг ответов
- [x] Контекст диалога в многоходовых запросах
- [x] Инкрементальная переобработка (diff-aware ingest)
- [x] Экспорт и импорт библиотек
- [x] Admin CLI (`ironrag-cli`) с гранулярными правами токенов
- [x] Ollama и локальные модели, проверено на живом Ollama с моделью `qwen3:4b`; отдельно проверен сценарий недоступной модели `qwen3:0.6b`.

## Contributing

PRs приветствуются. Один канонический путь вместо compatibility-слоёв.

## License

[MIT](./LICENSE)
