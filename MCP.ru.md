<div align="center">

# RustRAG MCP

### Подключите Codex, Cursor, VS Code, Claude Code или любой HTTP MCP-клиент к той же базе знаний, что использует RustRAG

[README.ru.md](./README.ru.md) | [MCP.md](./MCP.md)

</div>

## Endpoint

- JSON-RPC: `POST http://127.0.0.1:19000/v1/mcp`
- Capabilities: `GET http://127.0.0.1:19000/v1/mcp/capabilities`
- Заголовок авторизации: `Authorization: Bearer <token>`
- Имя MCP-сервера на протокольном уровне: `rustrag-mcp-memory`
- Имя клиента в готовых сниппетах админки: `rustragMemory`

Быстрая проверка:

```bash
export RUSTRAG_MCP_TOKEN='rtrg_...'

curl -sS http://127.0.0.1:19000/v1/mcp/capabilities \
  -H "Authorization: Bearer $RUSTRAG_MCP_TOKEN"
```

Если RustRAG стоит за прокси или под другим доменом, подставьте тот origin, который реально видит клиент.

## Подключение за минуту

1. Поднимите RustRAG через Docker Compose.
2. В `Admin -> Access` создайте API-токен и сразу сохраните plaintext secret.
3. Выдайте гранты на workspace, library или document, которые агент должен видеть.
4. В `Admin -> MCP` скопируйте готовый сниппет для клиента.

`tools/list` фильтруется грантами. Если токену что-то нельзя, инструмент просто не будет рекламироваться.

## Что умеют агенты

- `list_workspaces`, `list_libraries`
- `search_documents`, `read_document`
- `upload_documents`, `update_document`, `get_mutation_status`
- `create_workspace`, `create_library` при наличии админских прав

Под капотом MCP использует те же канонические сервисы, что и веб-приложение: Postgres для control state, ArangoDB для графа и документной истины, Redis-backed workers для ingestion.

## Модель доступа

- Токены можно ограничивать конкретными workspace и library.
- Read-only токены подходят для ассистентов, которым нужен только поиск и чтение.
- Write-enabled токены могут загружать документы и обновлять существующие материалы, если агенту нужно самому поддерживать knowledge base.
- Видимость инструментов следует за грантами: клиент видит только то, что ему разрешено.

## Что получает клиент

- Ту же searchable и grounded базу знаний, что использует встроенный ассистент в UI.
- То же каноническое состояние документов, которое используется для загрузок, обновлений, поиска и графовой навигации.
- Нормальный способ подключить внутреннего бота, саппорт-ассистента или персонального агента к управляемой knowledge base без отдельного адаптерного слоя.

## OpenAI Codex CLI

```bash
export RUSTRAG_MCP_TOKEN='rtrg_...'

codex mcp add rustragMemory \
  --url http://127.0.0.1:19000/v1/mcp \
  --bearer-token-env-var RUSTRAG_MCP_TOKEN
```

`~/.codex/config.toml`:

```toml
[mcp_servers.rustragMemory]
url = "http://127.0.0.1:19000/v1/mcp"
bearer_token_env_var = "RUSTRAG_MCP_TOKEN"
```

## VS Code или любой generic HTTP MCP client

`.vscode/mcp.json`:

```json
{
  "servers": {
    "rustragMemory": {
      "type": "http",
      "url": "http://127.0.0.1:19000/v1/mcp",
      "headers": {
        "Authorization": "Bearer ${env:RUSTRAG_MCP_TOKEN}"
      }
    }
  }
}
```

Если клиент умеет принимать сырой HTTP MCP-конфиг, достаточно URL endpoint и bearer token header.
