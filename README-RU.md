# RustRAG

### Система знаний для документов, внутренних ботов и AI-агентов в один клик

Загружайте файлы, ссылки и изображения, получайте searchable text, embeddings и граф связей, а затем используйте одну и ту же память и в UI, и через MCP.

[README](./README.md) • [MCP](./MCP.md) • [MCP-RU](./MCP-RU.md)



> RustRAG даёт практическую систему знаний для LLM: один `docker compose up`, один веб-интерфейс, один MCP endpoint и один канонический пайплайн для внутренних ассистентов, саппорта и приватных агентных сценариев.

## Почему RustRAG

- Поднимается быстро: ArangoDB, Postgres, Redis, nginx, SPA на React/Vite и Rust API (Axum) с MCP поднимаются одним стеком.
- Подходит для реальных knowledge-сценариев: документы и сайты ingest'ятся один раз, а дальше то же каноническое состояние используется для поиска, графа и агентного доступа.
- Даёт нормальную модель доступа: токены, гранты, ограничение по библиотекам и готовые MCP-сниппеты управляются из продукта.
- Полезен не только как демо: можно выбирать модели, смотреть затраты по документу, сайту или библиотеке и сразу проверять grounded-ответы во встроенном UI-ассистенте.

## Архитектура

Снаружи открыт **один порт** на **nginx**. `**/`** — SPA на **React + Vite** (статика из образа frontend); `**/v1/*`** — **Rust / Axum** (**REST + MCP**; запросы на `**/mcp`** перенаправляются на `**/v1/mcp**`). **Worker** — тот же образ backend, отдельная роль: потребитель очереди задач.

```text
                         ┌─────────────────────────┐
                         │   nginx (edge proxy)    │
                         │   single host:port      │
                         └───────────┬─────────────┘
               ┌─────────────────────┴─────────────────────┐
               │                                           │
        GET /* (SPA)                                 /v1/* (API + MCP)
               │                                           │
      ┌────────▼─────────┐                       ┌─────────▼──────────┐
      │    frontend      │                       │      backend       │
      │  React + Vite    │                       │   Rust / Axum      │
      │  static bundle   │                       │   API + MCP        │
      └──────────────────┘                       └─────────┬──────────┘
                                                           │
                         ┌─────────────────────────────────┼─────────────────────────────┐
                         │                                 │                             │
                  ┌──────▼──────┐                  ┌───────▼───────┐             ┌───────▼───────┐
                  │  ArangoDB   │                  │   Postgres    │             │    Redis      │
                  │ graph+vector│                  │ IAM + control │             │ worker queue  │
                  └─────────────┘                  └───────────────┘             └───────┬───────┘
                                                                                         │
                                                                                ┌────────▼────────┐
                                                                                │     worker      │
                                                                                │ (same image,    │
                                                                                │  ingest jobs)   │
                                                                                └─────────────────┘
```

Backend и worker оба используют Postgres, Redis, ArangoDB и общий том с контентом.

**Пайплайн ingestion:** загрузка → извлечение текста → чанкинг → эмбеддинги → слияние сущностей и связей → граф и поиск → UI оператора и инструменты MCP.

Одно и то же каноническое состояние документа затем используется и для поиска, и для чтения, и для обновлений, и для навигации по графу.

## Быстрый старт

Нужен Docker с Compose v2.
Для локальной сборки используется `Rust 1.94` (`1.94-trixie` в контейнерах).

Выберите один сценарий запуска:

### 1. Установить опубликованный релиз без клона репозитория

Последний релиз:

```bash
curl -fsSL https://raw.githubusercontent.com/mlimarenko/RustRAG/master/install.sh | bash
```

Конкретный тег:

```bash
curl -fsSL https://raw.githubusercontent.com/mlimarenko/RustRAG/master/install.sh | bash -s -- 0.1.0
```

Каталог `./rustrag` из релиза и старт стека. Первый `.env`: случайные `RUSTRAG_POSTGRES_PASSWORD`, `RUSTRAG_ARANGODB_PASSWORD`, `RUSTRAG_BOOTSTRAP_TOKEN`; следующие запуски их не меняют. В конце скрипта — кратко про админа и ключи LLM: в `.env` или в UI.

Занят `19000`: `RUSTRAG_PORT=8080 curl -fsSL https://raw.githubusercontent.com/mlimarenko/RustRAG/master/install.sh | bash`.

### 2. Запустить готовые образы из клонированного репозитория

```bash
cp .env.example .env
docker compose up -d
```

### 3. Собрать из исходников из клонированного репозитория

```bash
cp .env.example .env
docker compose -f docker-compose-local.yml up --build -d
```

### 4. Удалённый хост (Ansible)

Управляющая машина: Ansible. Цель: Docker, Compose v2, `curl`. Из клона:

```bash
ansible-playbook -i '203.0.113.10,' ansible/deploy.yml -b -u deploy \
  -e rustrag_install_dir=/opt/rustrag \
  -e rustrag_public_host=rag.example.com
```

Тег вместо `latest`:

```bash
ansible-playbook -i '203.0.113.10,' ansible/deploy.yml -b -e rustrag_release=0.1.0
```

На хосте выполняется [install.sh](./install.sh); повторный запуск — обновление. Доп. `-e`: `ansible/deploy.yml`.

Откройте:

- UI и API: [http://127.0.0.1:19000](http://127.0.0.1:19000)
- MCP JSON-RPC: `http://127.0.0.1:19000/v1/mcp`

Если нужен другой порт:

```bash
RUSTRAG_PORT=8080 docker compose up -d
```

На свежем стенде при первом открытии UI — bootstrap: логин и пароль администратора задаёте вы (дефолтного пароля для входа нет). `RUSTRAG_BOOTSTRAP_TOKEN` по умолчанию `bootstrap-local` — только для API/bootstrap, не пароль портала. По желанию: админ из env — `RUSTRAG_UI_BOOTSTRAP_ADMIN_LOGIN` / `RUSTRAG_UI_BOOTSTRAP_ADMIN_PASSWORD`.

## Модель конфигурации

У RustRAG один канонический стиль переменных приложения: `RUSTRAG_*`.

- `[.env.example](./.env.example)`: простой compose-ориентированный набор. Его копируют в `.env` для релизной установки, локальной сборки или внутреннего деплоя.
- `[apps/api/.env.example](./apps/api/.env.example)`: более полный справочник переменных API/worker для прямых запусков и тонкой настройки.
- `[docker-compose.yml](./docker-compose.yml)`: compose-поверхность по умолчанию на готовых Docker Hub образах.
- `[docker-compose-local.yml](./docker-compose-local.yml)`: compose-поверхность для ручной локальной сборки из исходников.
- `[apps/api/src/app/config.rs](./apps/api/src/app/config.rs)`: встроенные значения по умолчанию.

Нижний регистр, смешанные алиасы и ad-hoc варианты имён не поддерживаются.

## Где смотреть переменные

- Root `.env`: активный файл интерполяции для compose.
- `[./.env.example](./.env.example)`: минимальный compose-facing набор.
- `[./apps/api/.env.example](./apps/api/.env.example)`: более полный справочник runtime-конфигурации.
- `[./docker-compose.yml](./docker-compose.yml)`: дефолтная compose-поверхность на готовых образах.
- `[./docker-compose-local.yml](./docker-compose-local.yml)`: локальная compose-поверхность со сборкой из исходников.
- `[./apps/api/src/app/config.rs](./apps/api/src/app/config.rs)`: канонические имена настроек и встроенные дефолты.
- `docker compose config`: итоговый отрендеренный compose после подстановки `.env`.

## Релизные образы

- GitHub Releases публикуют `pipingspace/rustrag-backend:<tag>`, собранный из `apps/api`, в Docker Hub и обновляют тег `latest`.
- `[docker-compose.yml](./docker-compose.yml)` по умолчанию смотрит на этот релизный канал.
- При необходимости можно зафиксировать другой тег через `RUSTRAG_BACKEND_IMAGE` в `.env`.

## Стек


| Слой             | Технологии                                                |
| ---------------- | --------------------------------------------------------- |
| API + worker     | Rust, Axum, SQLx, асинхронные задачи (`apps/api/`)        |
| Фронтенд         | React, Vite, Tailwind CSS, shadcn/ui, Radix (`apps/web/`) |
| Граф и векторы   | ArangoDB 3.12 с экспериментальными векторными индексами   |
| Control plane    | PostgreSQL 18                                             |
| Очередь воркеров | Redis 8                                                   |
| Обратный прокси  | nginx 1.28                                                |
| Деплой           | Docker Compose, Ansible                                   |


Для локальной сборки из исходников: toolchain **Rust 1.94** (образ `rust:1.94-trixie`), корень workspace — `./` с единым `Cargo.toml`.

## Поддерживаемые входные данные

- Текстовые и text-like форматы: `txt`, `md`, `markdown`, `csv`, `json`, `yaml`, `yml`, `xml`, `log`, `rst`, `toml`, `ini`, `cfg`, `conf`
- Код и технические файлы: `ts`, `tsx`, `js`, `jsx`, `mjs`, `cjs`, `py`, `rs`, `java`, `kt`, `go`, `sh`, `sql`, `css`, `scss`
- Документы и страницы: `pdf`, `docx`, `pptx`, `html`, `htm`
- Изображения: `png`, `jpg`, `jpeg`, `gif`, `bmp`, `webp`, `svg`, `tif`, `tiff`, `heic`, `heif`
- Ссылки и веб-страницы тоже ingest'ятся прямо в ту же библиотеку, что и загруженные файлы.

## Эксплуатация и права

- Для разных этапов ingestion и query можно выбирать разные модели.
- Ведётся учёт затрат: видно, сколько ушло на обработку документа, web ingest по сайту или целой библиотеки.
- Гранты можно ограничивать так, чтобы агент видел только нужные библиотеки.
- Read-only MCP токены умеют читать и искать; write-enabled токены могут загружать и обновлять материалы, если вы хотите, чтобы агент сам поддерживал knowledge base в актуальном состоянии.
- Во встроенном ассистенте можно выбрать библиотеку и сразу проверить, как ведут себя grounded-ответы до подключения внешнего агента.

## Benchmark corpus для развёрнутого стенда

В репозитории есть нейтральный benchmark corpus на базе Wikipedia, Wikimedia Commons и synthetic multiformat fixtures. Его можно загрузить в уже поднятый стек и затем прогнать grounded QA matrix.

Подготовка:

```bash
cd /home/leader/sources/RustRAG/rustrag
export RUSTRAG_SESSION_COOKIE="..."
export RUSTRAG_BENCHMARK_WORKSPACE_ID="workspace-uuid"
```

Загрузить benchmark corpus в новую или существующую библиотеку:

```bash
make benchmark-grounded-seed
```

Что делает команда:

- создаёт новую benchmark-библиотеку, если `RUSTRAG_BENCHMARK_LIBRARY_ID` не задан
- загружает весь corpus из `apps/api/benchmarks/grounded_query/corpus`
- ждёт, пока библиотека станет читаемой и очередь успокоится
- пишет сводку в `tmp-grounded-benchmarks/upload.result.json`

Полезные переменные:

- `RUSTRAG_BENCHMARK_BASE_URL`: базовый API URL, по умолчанию `http://127.0.0.1:19000/v1`
- `RUSTRAG_BENCHMARK_WORKSPACE_ID`: UUID workspace, куда грузить benchmark corpus
- `RUSTRAG_SESSION_COOKIE`: значение cookie `rustrag_ui_session`
- `RUSTRAG_BENCHMARK_LIBRARY_NAME`: имя новой benchmark-библиотеки
- `RUSTRAG_BENCHMARK_LIBRARY_ID`: использовать уже существующую библиотеку вместо создания новой
- `RUSTRAG_BENCHMARK_SUITES`: ограничить набор suite-файлов, если нужен не весь corpus

Примеры:

```bash
# загрузить весь benchmark corpus
make benchmark-grounded-seed

# загрузить только multiformat fixtures в существующую библиотеку
make benchmark-grounded-seed \
  RUSTRAG_BENCHMARK_LIBRARY_ID="library-uuid" \
  RUSTRAG_BENCHMARK_SUITES="apps/api/benchmarks/grounded_query/multiformat_surface_suite.json"
```

После загрузки прогон полного benchmark matrix:

```bash
make benchmark-grounded-all
```

Подробности по составу corpus и suite-файлам есть в [apps/api/benchmarks/grounded_query/README.md](./apps/api/benchmarks/grounded_query/README.md).

## MCP для агентов

HTTP MCP встроен в продукт из коробки. Создайте токен в `Admin -> Access`, назначьте гранты и скопируйте готовый клиентский сниппет из `Admin -> MCP`.

Базовая поверхность инструментов включает `list_workspaces`, `list_libraries`, `search_documents`, `read_document`, `upload_documents`, `update_document` и `get_mutation_status`. Админские инструменты доступны только при нужных правах.

Быстрое подключение клиентов описано в [MCP-RU.md](./MCP-RU.md).

## Дорожная карта

### 0.2.0 — качество и производительность

- Гибридный поиск (BM25 + слияние с вектором) для лучшего извлечения
- Реранкер на cross-encoder для фильтрации top-K чанков
- Синхронизация сущностей с ArangoDB (запись сущностей из графового извлечения в ArangoDB)
- Стриминг ответов на запросы через SSE
- Контекст диалога в многоходовых запросах
- Инкрементальная переобработка (ingest с учётом diff для обновлённых документов)
- Параллельное графовое извлечение (параллельная обработка чанков)
- Учёт стоимости эмбеддингов (сейчас учитывается только графовое извлечение)
- Экспорт и импорт библиотек

### Дальше

- Поддержка извлечения из видео и аудио
- Определение языка и формата со стратегиями извлечения по типам
- Чанкинг с учётом кода (на AST для Rust, TypeScript, SQL, Python)
- Мультитенантная изоляция workspace
- RBAC с детальными правами на документы
- Пользовательские промпты извлечения на библиотеку
- Плагины для своих процессоров документов
- Поддержка Ollama и локальных моделей
- Коннекторы Confluence, Notion, Google Drive

## Contributing

Мы рады любым нормальным правкам: документации, UX, ingestion, MCP, тестам, фиксам и чистке лишнего.

Если меняете поведение или структуру, лучше сразу вести код к одному каноническому пути, а не добавлять совместимость, дубли или параллельные сценарии.