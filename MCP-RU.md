<div align="center">

# RustRAG MCP

### Подключите Codex, Cursor, VS Code, Claude Code или любой HTTP MCP-клиент к той же базе знаний, что использует RustRAG

[README-RU.md](./README-RU.md) | [MCP.md](./MCP.md)

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

## Инструменты (21)

### Обнаружение

| Инструмент | Описание | Обязательные параметры |
|------------|----------|----------------------|
| `list_workspaces` | Список workspace, видимых текущему токену. | (нет) |
| `list_libraries` | Список видимых библиотек с фильтрацией по workspace. | `workspaceId` (опц.) |

### Администрирование

| Инструмент | Описание | Обязательные параметры |
|------------|----------|----------------------|
| `create_workspace` | Создать workspace (только system admin). | `name` |
| `create_library` | Создать библиотеку внутри workspace. | `workspaceId`, `name` |

### Документы

| Инструмент | Описание | Обязательные параметры |
|------------|----------|----------------------|
| `search_documents` | Поиск по библиотеке: гибридный BM25 + вектор. Возвращает хиты на уровне документов. | `query` |
| `read_document` | Прочитать документ полностью или частями (с continuation token). | `documentId` |
| `list_documents` | Список документов в библиотеке с фильтрацией по статусу. | `libraryId` |
| `upload_documents` | Загрузить один или несколько документов. Поддерживает base64 и inline-текст. | `libraryId`, `documents` |
| `update_document` | Дописать или заменить содержимое документа. | `libraryId`, `documentId`, `operationKind` |
| `delete_document` | Удалить документ вместе с ревизиями, чанками и вкладом в граф. | `documentId` |
| `get_mutation_status` | Проверить статус мутации (upload/update/delete). | `receiptId` |

### Граф знаний

| Инструмент | Описание | Обязательные параметры |
|------------|----------|----------------------|
| `ask` | Задать grounded-вопрос к библиотеке. Возвращает синтезированный ответ с верификацией. | `libraryId`, `question` |
| `search_entities` | Поиск сущностей в графе знаний по имени или описанию. | `libraryId`, `query` |
| `get_graph_topology` | Получить топологию графа (сущности, связи, документные привязки) с лимитом. | `libraryId` |
| `list_relations` | Список связей в графе, упорядоченных по количеству подтверждений. | `libraryId` |

### Веб-краулинг

| Инструмент | Описание | Обязательные параметры |
|------------|----------|----------------------|
| `submit_web_ingest_run` | Запустить ingestion с веб-страницы или рекурсивный краул сайта. | `libraryId`, `seedUrl`, `mode` |
| `get_web_ingest_run` | Загрузить текущий статус веб-краулинга. | `runId` |
| `list_web_ingest_run_pages` | Список обнаруженных страниц и их статусов. | `runId` |
| `cancel_web_ingest_run` | Отменить активный веб-краулинг. | `runId` |

### Runtime

| Инструмент | Описание | Обязательные параметры |
|------------|----------|----------------------|
| `get_runtime_execution` | Загрузить summary жизненного цикла runtime-исполнения. | `executionId` |
| `get_runtime_execution_trace` | Полная трассировка стадий, действий и policy-решений. | `executionId` |

Под капотом MCP использует те же канонические сервисы, что и веб-приложение: Postgres для control state, ArangoDB для графа и документной истины, Redis-backed workers для ingestion.

## Модель доступа

- Токены можно ограничивать конкретными workspace и library.
- Read-only токены подходят для ассистентов, которым нужен только поиск, чтение и Q&A.
- Write-enabled токены могут загружать, обновлять и удалять документы, если агенту нужно самому поддерживать knowledge base.
- Видимость инструментов следует за грантами: клиент видит только то, что ему разрешено.

## Что получает клиент

- Ту же searchable и grounded базу знаний, что использует встроенный ассистент в UI.
- Граф знаний с типизированными сущностями (person, organization, artifact, natural, process, concept и др.) и 88 типами связей.
- Гибридный поиск (BM25 + vector) с учётом quality score чанков и field-weighted scoring заголовков.
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
