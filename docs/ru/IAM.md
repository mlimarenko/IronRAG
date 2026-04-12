# IronRAG IAM — Идентификация, доступ и токены

[README-RU](../../README-RU.md) | [CLI](./CLI.md) | [MCP](./MCP.md)

## Основные понятия

| Термин | Описание |
|--------|----------|
| **Principal** | Любая сущность, которая может аутентифицироваться: пользователь (логин + пароль) или API-токен. |
| **Грант** | Запись, связывающая principal с правом доступа в определённом скоупе. |
| **Скоуп** | Граница действия гранта: `system`, `workspace`, `library` или `document`. |
| **Право (permission kind)** | Именованная возможность (например `library_read`, `query_run`). |
| **Токен** | Bearer-секрет с префиксом `irt_`. Создаётся через CLI или Admin UI. Показывается один раз. |

## Виды прав

| Право | Описание | Типичное использование |
|-------|----------|----------------------|
| `iam_admin` | Полное администрирование системы. Подразумевает все остальные права. | Системный оператор |
| `workspace_admin` | Управление настройками workspace, библиотеками, участниками. | Владелец workspace |
| `workspace_read` | Просмотр метаданных workspace. | Чтение workspace |
| `library_read` | Чтение документов, список библиотек, поиск, граф. | Read-only агент |
| `library_write` | Загрузка, обновление, удаление документов. Подразумевает видимость `library_read`. | Write-агент |
| `document_read` | Чтение конкретного документа (гранулярный доступ). | Доступ к одному документу |
| `document_write` | Запись в конкретный документ (гранулярный доступ). | Запись в один документ |
| `query_run` | Выполнение запросов `ask` к библиотекам. | Q&A-агент |
| `ops_read` | Просмотр статуса и трассировок runtime-исполнений. | Мониторинг |
| `audit_read` | Просмотр записей аудита. | Compliance |
| `credential_admin` | Управление credentials провайдеров (API-ключи). | Управление секретами |
| `binding_admin` | Управление привязками моделей к библиотекам. | Конфигурация моделей |

## Скоупы грантов

Гранты иерархичны. Более широкий скоуп неявно покрывает более узкие:

```
system (все workspace)
  └── workspace (все библиотеки в этом workspace)
        └── library (все документы в этой библиотеке)
              └── document (один конкретный документ)
```

| Скоуп | Значение | Пример |
|-------|----------|--------|
| `system` | Право распространяется на все ресурсы инстанса. | `iam_admin` на `system` = полный админ |
| `workspace` | Право распространяется на все библиотеки и документы в workspace. | `library_read` на `workspace:default` |
| `library` | Право распространяется на все документы в библиотеке. | `library_write` на `library:docs` |
| `document` | Право распространяется на один конкретный документ. | `document_read` на `document:<uuid>` |

## Матрица видимости MCP-инструментов

MCP `tools/list` возвращает только те инструменты, которые разрешены правами токена:

| Инструмент | Необходимое право (любое из) |
|------------|------------------------------|
| `list_workspaces` | `workspace_read`, `workspace_admin`, `library_read`, `library_write`, `query_run`, `iam_admin` |
| `list_libraries` | `library_read`, `library_write`, `workspace_admin`, `iam_admin` |
| `create_workspace` | `workspace_admin`, `iam_admin` |
| `create_library` | `workspace_admin`, `iam_admin` |
| `search_documents` | `document_read`, `library_read`, `library_write`, `workspace_admin`, `iam_admin` |
| `read_document` | `document_read`, `library_read`, `library_write`, `workspace_admin`, `iam_admin` |
| `list_documents` | `document_read`, `library_read`, `library_write`, `workspace_admin`, `iam_admin` |
| `ask` | `query_run`, `library_read`, `workspace_admin`, `iam_admin` |
| `upload_documents` | `document_write`, `library_write`, `workspace_admin`, `iam_admin` |
| `update_document` | `document_write`, `library_write`, `workspace_admin`, `iam_admin` |
| `delete_document` | `document_write`, `library_write`, `workspace_admin`, `iam_admin` |
| `get_mutation_status` | `document_write`, `library_write`, `workspace_admin`, `iam_admin` |
| `search_entities` | `library_read`, `library_write`, `workspace_admin`, `iam_admin` |
| `get_graph_topology` | `library_read`, `library_write`, `workspace_admin`, `iam_admin` |
| `list_relations` | `library_read`, `library_write`, `workspace_admin`, `iam_admin` |
| `get_communities` | `library_read`, `library_write`, `workspace_admin`, `iam_admin` |
| `submit_web_ingest_run` | `library_write`, `workspace_admin`, `iam_admin` |
| `cancel_web_ingest_run` | `library_write`, `workspace_admin`, `iam_admin` |
| `get_web_ingest_run` | `library_read`, `library_write`, `workspace_admin`, `iam_admin` |
| `list_web_ingest_run_pages` | `library_read`, `library_write`, `workspace_admin`, `iam_admin` |
| `get_runtime_execution` | `ops_read`, `library_read`, `workspace_admin`, `iam_admin` |
| `get_runtime_execution_trace` | `ops_read`, `library_read`, `workspace_admin`, `iam_admin` |

## Готовые профили токенов

Типовые конфигурации для разных типов агентов:

### Полный админ

```bash
ironrag-cli create-token admin
# Гранты: iam_admin (system)
# Инструменты: 22 (все)
```

### Read-only агент

```bash
ironrag-cli create-token admin -p library_read -p query_run -l "reader"
# Инструменты: 14 — list, search, read, ask, граф, runtime
```

### Write-агент

```bash
ironrag-cli create-token admin -p library_read -p library_write -l "writer"
# Инструменты: 20 — всё кроме create_workspace и create_library
```

### Reader в рамках workspace

```bash
ironrag-cli create-token admin -p library_read -p query_run -w default -l "ws-reader"
# Инструменты: 14 — как reader, но только для библиотек в workspace "default"
```

### Мониторинг / ops

```bash
ironrag-cli create-token admin -p ops_read -p audit_read -l "monitoring"
# Инструменты: 2 — list_workspaces, list_libraries
```

### Загрузчик документов (без чтения и запросов)

```bash
ironrag-cli create-token admin -p library_write -l "uploader"
# Инструменты: 20 — может загружать/обновлять/удалять, а также читать (library_write подразумевает видимость документов)
```

## Жизненный цикл токена

1. **Создание**: через CLI (`ironrag-cli create-token`) или Admin UI. Токен в открытом виде показывается один раз.
2. **Хранение**: в БД хранится только SHA-256 хеш в таблице `iam_api_token`. Префикс (`irt_`) и первые 8 символов сохраняются для идентификации.
3. **Аутентификация**: клиент передаёт `Authorization: Bearer irt_...`. Бэкенд хеширует токен и находит соответствующий principal.
4. **Разрешение грантов**: все гранты principal загружаются с материализованными workspace/library/document ID через JOIN.
5. **Авторизация**: каждая операция API/MCP проверяет гранты на соответствие требуемым правам и целевому скоупу ресурса.
6. **Отзыв**: через CLI (`ironrag-cli revoke-token <principal-id>`) или Admin UI. Устанавливает статус `revoked`. Вступает в силу немедленно.

## HTTP API аутентификация

Все `/v1/*` эндпоинты принимают bearer-токены:

```bash
curl -H "Authorization: Bearer irt_..." http://localhost:19000/v1/workspaces
```

Session cookies (от логина) и API-токены используют одну систему авторизации. Session-токены создаются через `POST /v1/iam/sessions` (endpoint логина).

## Безопасность

- Токены хешируются SHA-256 перед сохранением. Plaintext никогда не персистится.
- Пароли хешируются Argon2id.
- System-scoped гранты дают доступ ко всем ресурсам. Используйте workspace или library скоупы для принципа наименьших привилегий.
- Право `iam_admin` особое: оно устанавливает `is_system_admin=true`, обходя все проверки по конкретным ресурсам.
- Истекшие гранты (`expires_at < now()`) автоматически исключаются из авторизации.
