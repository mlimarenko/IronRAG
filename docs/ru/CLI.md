# IronRAG CLI

[Обзор](./README.md) | [IAM](./IAM.md) | [MCP](./MCP.md)

Инструмент командной строки для административных операций IronRAG. Подключается напрямую к PostgreSQL.

## Сборка

```bash
cargo build --release -p ironrag-backend --bin ironrag-cli
```

Бинарный файл также включен в Docker-образ по пути `/usr/local/bin/ironrag-cli`.

## Конфигурация

CLI использует те же переменные окружения, что и сервер. Обязательная переменная -- `DATABASE_URL` (или эквивалентная настройка из конфигурации приложения).

## Команды

### Версия CLI

```bash
ironrag-cli version
```

Выводит версию сборки CLI (совпадает с версией крейта `ironrag-backend`).

### Список пользователей

```bash
ironrag-cli list-users
```

Выводит таблицу всех пользователей с логином, отображаемым именем, статусом и датой создания.

### Создание пользователя

```bash
ironrag-cli create-user <LOGIN> <PASSWORD> [--name "Отображаемое имя"]
```

Создает нового пользователя с правами администратора (грант `iam_admin`). Пользователь автоматически добавляется в workspace по умолчанию, если он существует. Пароль должен содержать не менее 8 символов.

Параметры:
- `-n, --name` -- отображаемое имя (по умолчанию используется логин)

### Сброс пароля

```bash
ironrag-cli reset-password <LOGIN> <PASSWORD>
```

Обновляет пароль существующего пользователя и отзывает все активные сессии, требуя повторной аутентификации. Пароль должен содержать не менее 8 символов.

### Удаление пользователя

```bash
ironrag-cli delete-user <LOGIN>
```

Безвозвратно удаляет пользователя и все связанные записи (сессии, гранты, членство в workspace, principal).

### Создание API-токена

```bash
ironrag-cli create-token <LOGIN> [--label "my-token"] [--workspace "my-workspace"] [--permission <PERM>...] [--scope <SCOPE>]
```

Создает API-токен, привязанный к указанному пользователю. Токен в открытом виде отображается один раз и не может быть получен повторно. Токены имеют префикс `irt_`.

Параметры:
- `-l, --label` -- метка токена (по умолчанию `api-token`)
- `-w, --workspace` -- ограничить токен конкретным workspace (по slug или UUID)
- `-p, --permission` -- право доступа (можно указать несколько раз). Без указания по умолчанию `iam_admin`
- `--scope` -- явный скоуп гранта: `system`, `workspace:<slug>` или `library:<slug>`

Доступные права:
- `iam_admin` -- полное администрирование системы
- `workspace_admin`, `workspace_read` -- управление workspace
- `library_read`, `library_write` -- доступ к библиотекам и документам
- `document_read`, `document_write` -- доступ на уровне документа
- `query_run` -- выполнение запросов (ask)
- `ops_read`, `audit_read` -- операционные и аудит данные
- `credential_admin`, `binding_admin` -- управление интеграциями
- `connector_admin` -- управление коннекторами

Разрешение скоупа (когда `--scope` не указан):
- Системные права (`iam_admin`, `ops_read`, `audit_read`) → скоуп `system`
- Остальные права с `--workspace` → скоуп `workspace` на указанный workspace
- Остальные права без `--workspace` → скоуп `system` (доступ ко всем workspace)

Примеры:
```bash
# Полный админ-токен
ironrag-cli create-token admin

# Токен только на чтение для всех workspace
ironrag-cli create-token admin -p library_read -p query_run -l "reader"

# Токен на запись в конкретный workspace
ironrag-cli create-token admin -p library_read -p library_write -w default -l "writer"

# Токен для мониторинга
ironrag-cli create-token admin -p ops_read -p audit_read -l "monitoring"
```

### Список API-токенов

```bash
ironrag-cli list-tokens
```

Выводит все API-токены с principal ID, меткой, префиксом, статусом, датой выпуска и владельцем.

### Отзыв API-токена

```bash
ironrag-cli revoke-token <TOKEN_PRINCIPAL_ID>
```

Отзывает API-токен по UUID его principal. Устанавливает статус токена и principal в `revoked`.

### Список workspace

```bash
ironrag-cli list-workspaces
```

Выводит все workspace с ID, slug, отображаемым именем, состоянием жизненного цикла и датой создания.

### Создание workspace

```bash
ironrag-cli create-workspace <SLUG> [--name "Отображаемое имя"]
```

Создает новый workspace.

Параметры:
- `-n, --name` -- отображаемое имя (по умолчанию используется slug)

### Список библиотек

```bash
ironrag-cli list-libraries <WORKSPACE>
```

Выводит все библиотеки в workspace. Workspace можно указать по slug или UUID.

### Создание библиотеки

```bash
ironrag-cli create-library <WORKSPACE> <SLUG> [--name "Отображаемое имя"] [--description "Описание"]
```

Создает новую библиотеку в указанном workspace.

Параметры:
- `-n, --name` -- отображаемое имя (по умолчанию используется slug)
- `-d, --description` -- описание библиотеки

## Использование в Docker

```bash
docker exec <container> ironrag-cli list-users
docker exec <container> ironrag-cli create-user admin2 secretpass --name "Второй админ"
docker exec <container> ironrag-cli reset-password admin newpassword123
docker exec <container> ironrag-cli delete-user old-admin

docker exec <container> ironrag-cli create-token admin --label "ci-token" --workspace default
docker exec <container> ironrag-cli list-tokens
docker exec <container> ironrag-cli revoke-token <TOKEN_PRINCIPAL_ID>

docker exec <container> ironrag-cli list-workspaces
docker exec <container> ironrag-cli create-workspace staging --name "Staging"

docker exec <container> ironrag-cli list-libraries default
docker exec <container> ironrag-cli create-library default docs --name "Документация" --description "Публичная документация"
```
