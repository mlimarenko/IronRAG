# IronRAG IAM

[Обзор](./README.md) | [MCP](./MCP.md) | [CLI](./CLI.md)

IronRAG использует одну модель авторизации для web UI, HTTP API, токенов, созданных через CLI, и MCP tools.

## Базовые понятия

| Термин | Смысл |
|---|---|
| Principal | аутентифицированная сущность: пользователь или API token |
| Grant | разрешение, назначенное на конкретный scope |
| Scope | `system`, `workspace`, `library` или `document` |
| Permission kind | именованная capability вроде `library_read` или `iam_admin` |
| Token | bearer secret с префиксом `irt_` |

## Permission kinds

| Permission | Назначение |
|---|---|
| `iam_admin` | полное системное администрирование |
| `workspace_admin` | управление workspace и library |
| `workspace_read` | чтение metadata workspace |
| `library_read` | чтение library, documents, graph и связанных read-surface |
| `library_write` | upload, update, delete и web-ingest контента |
| `document_read` | чтение конкретного документа |
| `document_write` | мутация конкретного документа |
| `query_run` | запуск assistant и query turn |
| `ops_read` | чтение runtime и operational state |
| `audit_read` | чтение audit events |
| `credential_admin` | управление provider credentials |
| `binding_admin` | управление model bindings и presets |
| `connector_admin` | управление коннекторами |

## Иерархия permission

- `iam_admin` подразумевает `workspace_admin`, который подразумевает все остальные permission.
- `library_write` подразумевает `library_read` плюс document read/write.

## Иерархия scope

Более широкий scope покрывает более узкий ресурс:

```text
system
  -> workspace
    -> library
      -> document
```

**System** scope (`workspace_id=null`) дает полный admin-доступ ко всем workspace. Токен, выпущенный на system scope, не привязан к конкретному workspace.

Примеры:

- `library_read` на workspace покрывает все library внутри него.
- `document_write` на одном документе не дает доступа к соседним документам.
- `iam_admin` на `system` обходит per-resource checks.

## Session и token surface

Session routes:

- `POST /v1/iam/session/login`
- `GET /v1/iam/session/resolve`
- `POST /v1/iam/session/logout`

Bootstrap routes:

- `GET /v1/iam/bootstrap/status`
- `POST /v1/iam/bootstrap/setup`

API tokens проходят те же authorization checks, что и session-authenticated users.

## Жизненный цикл токена

1. Создай токен в Admin UI или через `ironrag-cli create-token`.
2. Скопируй plaintext token один раз.
3. Backend хранит только hash токена.
4. Клиент аутентифицируется через `Authorization: Bearer irt_...`.
5. Для каждого HTTP или MCP call grants разрешаются относительно целевого scope.

## MCP visibility model

`tools/list` фильтруется по grant'ам.

- Discovery и read tools требуют read-level access к целевому scope.
- Document mutation и web-ingest tools требуют write-level access.
- Runtime tools требуют `ops_read` или более сильного доступа, который уже покрывает target library.
- Catalog creation tools требуют admin-level access.

Если токен не может использовать tool, этот tool просто не будет рекламироваться.

Если токен ограничен ровно одним workspace или library, MCP tools и query API автоматически подставляют `workspace_id` и `library_id` из scope токена.

## Правила безопасности

- Токены хешируются до сохранения.
- Пароли хешируются через Argon2id.
- Просроченные grants игнорируются.
- System-scoped admin access намеренно широкий; по возможности используй workspace или library scope.
