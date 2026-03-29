

# RustRAG

### Локальная graph-memory платформа для документов и AI-агентов в один клик

Загружайте файлы, получайте searchable text, embeddings и граф связей, а затем используйте одну и ту же память и в UI, и через MCP.

[README](./README.md) • [MCP](./MCP.md) • [MCP.ru](./MCP.ru.md)







> RustRAG по сути даёт локальную graph-backed document database и memory layer для документов и AI-агентов: один `docker compose up`, один веб-интерфейс, один MCP endpoint и один канонический пайплайн без раздвоения логики.

## Почему RustRAG

- Поднимается быстро: ArangoDB, Postgres, Redis, Rust-сервисы, UI и MCP стартуют одним локальным стеком.
- Даёт graph-backed memory для документов: загрузки превращаются в чанки, эмбеддинги, сущности, связи и provenance, которые потом можно просматиривать и в UI, и отдавать ИИ агентам как базу знаний.
- Люди и агенты работают с одним состоянием: оператор через UI, агент через MCP, но память у них общая.
- Готов к реальной эксплуатации: токены, гранты, модельные настройки и MCP-сниппеты управляются из продукта.

## Быстрый старт

Нужен Docker с Compose v2.

```bash
docker compose up --build -d
```

Откройте:

- UI и API: [http://127.0.0.1:19000](http://127.0.0.1:19000)
- MCP JSON-RPC: `http://127.0.0.1:19000/v1/mcp`

Если нужен другой порт:

```bash
RUSTRAG_PORT=8080 docker compose up -d
```

На свежем стенде при первом открытии UI — bootstrap: логин и пароль администратора задаёте вы (дефолтного пароля для входа нет). `RUSTRAG_BOOTSTRAP_TOKEN` по умолчанию `bootstrap-local` — только для API/bootstrap, не пароль портала. По желанию: админ из env — `RUSTRAG_UI_BOOTSTRAP_ADMIN_LOGIN` / `RUSTRAG_UI_BOOTSTRAP_ADMIN_PASSWORD`.

## Стек

- Rust backend + worker для ingestion, graph build, query, IAM и MCP.
- ArangoDB для графа, документной памяти и vector-backed retrieval.
- Postgres для control plane, IAM, аудита, биллинга и состояния операций.
- Redis для координации воркеров.
- Vue 3 + Quasar frontend за Nginx.

## Как работает пайплайн

```text
upload -> text extraction -> chunking -> embeddings -> entity/relation merge -> graph + search -> UI and MCP
```

Одно и то же каноническое состояние документа затем используется и для поиска, и для чтения, и для обновлений, и для навигации по графу.

## MCP для агентов

HTTP MCP встроен в продукт из коробки. Создайте токен в `Admin -> Access`, назначьте гранты и скопируйте готовый клиентский сниппет из `Admin -> MCP`.

Базовая поверхность инструментов включает `list_workspaces`, `list_libraries`, `search_documents`, `read_document`, `upload_documents`, `update_document` и `get_mutation_status`. Админские инструменты доступны только при нужных правах.

Быстрое подключение клиентов описано в [MCP.ru.md](./MCP.ru.md).

## Contributing

Мы рады любым нормальным правкам: документации, UX, ingestion, MCP, тестам, фиксам и чистке лишнего.

Если меняете поведение или структуру, лучше сразу вести код к одному каноническому пути, а не добавлять совместимость, дубли или параллельные сценарии.