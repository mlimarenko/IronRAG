# RustRAG

### Система знаний для документов, внутренних ботов и AI-агентов

[README](./README.md) | [MCP](./MCP.md) | [MCP-RU](./MCP-RU.md)

> Один `docker compose up` — веб-интерфейс, MCP endpoint и канонический пайплайн для внутренних ассистентов, саппорта и агентных сценариев.

## Архитектура

Снаружи один порт на **nginx**. `/` — SPA (**React + Vite**); `/v1/*` — **Rust/Axum** backend (**REST + MCP**). **Worker** — тот же образ, роль потребителя очереди.

```text
                         ┌─────────────────────────┐
                         │   nginx (edge proxy)    │
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
                                                                        ┌───────▼───────┐
                                                                        │    worker     │
                                                                        └───────────────┘
```

## Пайплайн

```text
загрузка / URL → извлечение текста → структурные блоки → фильтр boilerplate
  → семантический чанкинг (2800 символов, 10% overlap, heading-aware)
  → эмбеддинги → графовое извлечение (v6: 10 типов сущностей, 88 типов связей)
  → резолюция сущностей (alias/acronym merge) → суммаризация документа
  → scoring качества → гибридный индекс (BM25 + vector) → UI + MCP + API
```

## Быстрый старт

Нужен Docker с Compose v2.

```bash
# Установка без клона репозитория
curl -fsSL https://raw.githubusercontent.com/mlimarenko/RustRAG/master/install.sh | bash

# Или из клонированного репозитория
cp .env.example .env
docker compose up -d          # готовые образы
# docker compose -f docker-compose-local.yml up --build -d  # сборка из исходников
```

После старта: [http://127.0.0.1:19000](http://127.0.0.1:19000). При первом входе — bootstrap: логин и пароль задаёте вы.

Другой порт: `RUSTRAG_PORT=8080 docker compose up -d`

## Возможности

- **Ingestion** — текст, код (50+ расширений), PDF, DOCX, PPTX, HTML, изображения, веб-страницы
- **Типизированный граф знаний** — 10 универсальных типов сущностей (person, organization, location, event, artifact, natural, process, concept, attribute, entity), 88 типов связей, entity resolution
- **Гибридный поиск** — BM25 + vector cosine через Reciprocal Rank Fusion, field-weighted scoring (заголовки 1.5x)
- **Grounded-ассистент** — встроенный чат с верификацией ответов и evidence panel
- **21 MCP-инструмент** — Q&A (`ask`), поиск, чтение, загрузка, граф, веб-краулинг, администрирование
- **Умный чанкинг** — 2800 символов, 10% overlap, heading-aware, code-aware, boilerplate detection, quality scoring
- **Доступ и контроль** — API-токены, гранты по библиотекам, ready-made MCP-сниппеты
- **Учёт затрат** — стоимость обработки по документу и библиотеке
- **Выбор моделей** — разные провайдеры и модели для каждого этапа пайплайна

## MCP для агентов

21 инструмент из коробки. Токен → гранты → сниппет из `Admin -> MCP`.

| Категория | Инструменты |
|-----------|-------------|
| **Q&A** | `ask` — grounded-вопросы к библиотеке |
| **Документы** | `search_documents`, `read_document`, `list_documents`, `upload_documents`, `update_document`, `delete_document` |
| **Граф** | `search_entities`, `get_graph_topology`, `list_relations` |
| **Веб-краулинг** | `submit_web_ingest_run`, `get_web_ingest_run`, `cancel_web_ingest_run` |
| **Обнаружение** | `list_workspaces`, `list_libraries` |

Ответы search/read по умолчанию используют `includeReferences=false` для минимизации токенов. Подробнее: [MCP-RU.md](./MCP-RU.md).

## Стек

| Слой | Технологии |
|------|-----------|
| API + worker | Rust, Axum, SQLx |
| Фронтенд | React, Vite, Tailwind, shadcn/ui |
| Граф и векторы | ArangoDB 3.12 |
| Control plane | PostgreSQL 18 |
| Очередь | Redis 8 |
| Прокси | nginx 1.28 |
| Деплой | Docker Compose, Ansible |

## Конфигурация

Все переменные — `RUSTRAG_*`. Ключевые файлы:

| Файл | Назначение |
|------|-----------|
| `.env.example` | Compose-переменные |
| `apps/api/.env.example` | Полный справочник runtime-конфигурации |
| `apps/api/src/app/config.rs` | Встроенные дефолты |

## Бенчмарки

В репозитории два golden dataset: Wikipedia corpus (30 вопросов) и code corpus (20 вопросов, 8 файлов на Go/TS/Python/Rust/Terraform/React/K8s/Docker).

```bash
export RUSTRAG_SESSION_COOKIE="..."
export RUSTRAG_BENCHMARK_WORKSPACE_ID="workspace-uuid"
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
- [x] 21 MCP-инструмент включая `ask` и граф-навигацию
- [x] Типизированная раскраска и подписи рёбер в UI графа
- [x] Параллельное извлечение (до 8 конкурентных чанков)
- [ ] SSE-стриминг ответов
- [ ] Контекст диалога в многоходовых запросах
- [ ] Инкрементальная переобработка (diff-aware ingest)
- [ ] Экспорт и импорт библиотек
- [ ] Ollama и локальные модели
- [ ] Коннекторы Confluence, Notion, Google Drive

## Contributing

PRs приветствуются. Один канонический путь вместо compatibility-слоёв.

## License

[MIT](./LICENSE)
