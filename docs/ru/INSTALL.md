<div align="center">

# Установка и начальная настройка

### Интерактивная и автоматическая установка, ключи провайдеров и телеметрия

[Обзор](./README.md) | [English](../en/INSTALL.md) | [AI bindings](./AI-BINDINGS.md) | [Шифрование credential](./CREDENTIAL-ENCRYPTION.md) | [Capacity planning](./CAPACITY-PLANNING.md)

</div>

## Интерактивная установка

```bash
curl -fsSL https://raw.githubusercontent.com/mlimarenko/IronRAG/master/install.sh | bash
```

Установщик проверяет ресурсы хоста, рекомендует профиль с ограничениями памяти,
CPU и параллелизма загрузки, затем запрашивает порт и начальную учётную запись
администратора. Перед записью он показывает итоговую конфигурацию. Ключи
провайдеров задаются переменными окружения ниже или в интерфейсе. При повторном
запуске сохраняются существующие секреты `.env` и выбранные ограничения, а
образы IronRAG обновляются до выбранного тега версии.

Полный список флагов и переменных — `./install.sh --help`; `--plan-only`
печатает обнаруженный профиль без записи и деплоя.

## Из исходников

```bash
git clone https://github.com/mlimarenko/IronRAG.git
cd IronRAG
# Только для первой установки: API и worker не стартуют без ключа шифрования
# учётных данных, а .env.example поставляет его пустым.
# В этот же файл добавьте IRONRAG_AI_PROVIDER_API_KEYS_JSON_B64.
if [ ! -f .env ]; then
  sed "s|^IRONRAG_CREDENTIAL_MASTER_KEY=.*|IRONRAG_CREDENTIAL_MASTER_KEY=$(openssl rand -base64 32)|" \
    .env.example > .env
fi
docker compose up -d
```

Не переносите этот ключ между развёртываниями, не выводите его из другого
секрета и не теряйте: им расшифровываются сохранённые учётные данные AI-аккаунтов
и вебхуков ([Шифрование учётных данных](./CREDENTIAL-ENCRYPTION.md)).
[Интерактивный установщик](#интерактивная-установка) создаёт такой ключ для
нового `.env` сам. Не перезаписывайте существующий `.env` и не пересоздавайте его
ключ, пока сохраняется том PostgreSQL развёртывания: сохранённые учётные данные
останутся нерасшифровываемыми. Чтобы изменить порт, профиль ресурсов, ключи
провайдеров или bootstrap-админа в существующей установке, повторно запустите
`install.sh` — он сохраняет уже записанные секреты.

Откройте [http://127.0.0.1:19000](http://127.0.0.1:19000), создайте admin-аккаунт,
загрузите документ и задайте вопрос.

## Брендинг фронтенда

Чтобы изменить название развёртывания, логотип, компактную иконку и favicon без
пересборки образа, смонтируйте runtime-файл `branding.json` и изображения с того
же origin. JSON-контракт и Compose mounts описаны в разделе
[Брендинг развёртывания](./FRONTEND-TRANSPORT.md#брендинг-развёртывания).
По умолчанию остаётся IronRAG; брендинг не меняет права и аутентификацию.

## Автоматическая установка

Без терминала (форма `curl | bash`) или с `--yes` / `--non-interactive`
установщик берёт ответы из флагов, переменных окружения и существующего
`.env`. Приоритет для не-секретов: flags > env > prompt. Секреты (admin
password, provider API keys) принимаются только через переменные окружения
или заранее заполненный `.env` — никогда через флаги: argv виден другим
процессам и попадает в history shell.

```bash
curl -fsSL https://raw.githubusercontent.com/mlimarenko/IronRAG/master/install.sh -o install.sh
IRONRAG_PORT=8080 \
IRONRAG_PROFILE=small \
IRONRAG_AI_PROVIDER_API_KEYS_JSON_B64="$(printf '%s' '{"provider-alpha":"secret-value"}' | base64 | tr -d '\n')" \
  bash install.sh --non-interactive
```

## OIDC SSO

IronRAG использует confidential server-side Authorization Code client с PKCE.
Зарегистрируйте у identity provider один точный HTTPS callback и настройте все
переменные вместе. Если все пять обязательных переменных пусты, SSO выключен;
частичная конфигурация отклоняется при startup.

```env
IRONRAG_OIDC_ISSUER_URL=https://id.example.com/realms/example
IRONRAG_OIDC_CLIENT_ID=ironrag
IRONRAG_OIDC_CLIENT_SECRET=<secret-manager-reference>
IRONRAG_OIDC_REDIRECT_URL=https://ironrag.example.com/v1/iam/oidc/callback
IRONRAG_OIDC_REQUIRED_GROUP=ironrag
```

Например, для деплоя на `https://ironrag.example.com` зарегистрируйте ровно
`https://ironrag.example.com/v1/iam/oidc/callback` как client redirect URI и
добавьте `https://ironrag.example.com` в `IRONRAG_FRONTEND_ORIGIN`. Не
используйте wildcard redirect.

`IRONRAG_OIDC_CLIENT_SECRET` — runtime credential. В managed deployment
храните его в Vault или другом secret manager и инжектируйте как environment
variable; не коммитьте в `.env`, не передавайте через command line и не
встраивайте в image. Identity provider должен включать настроенную required
group в проверенные user claims. Эта group допускает пользователя в IronRAG,
но не выдаёт роль приложения IronRAG.

Чтобы сделать один SSO identity первым локальным администратором IronRAG,
задайте необязательный `IRONRAG_OIDC_BOOTSTRAP_ADMIN_SUBJECT` равным
проверенному неизменяемому `sub` identity provider *до первого входа этого
пользователя в IronRAG*. Это выдаёт локальную admin-роль только при первом JIT
provisioning. Все остальные допущенные SSO users начинают с локальной ролью
`viewer`; не используйте email для bootstrap или связывания account'ов. Роли и
правила токенов описаны в [IAM.md](./IAM.md).

Отдельный break-glass password administrator остаётся доступен через
`IRONRAG_UI_BOOTSTRAP_ADMIN_LOGIN`, `IRONRAG_UI_BOOTSTRAP_ADMIN_EMAIL` и
`IRONRAG_UI_BOOTSTRAP_ADMIN_PASSWORD`. Храните этот пароль также в secret
manager. Startup сверяет его только для точного настроенного локального
password administrator, никогда не для SSO user.

## Ключи провайдеров в `.env`

Соберите нужные ключи в один JSON-map, закодируйте standard base64 и положите
в `.env`:

```env
IRONRAG_CREDENTIAL_MASTER_KEY=<padded-standard-base64-of-32-random-bytes>
# Опционально; по умолчанию `default`.
IRONRAG_CREDENTIAL_MASTER_KEY_ID=current-2026
# Только при ротации; ID ключей должны быть отсортированы и уникальны.
IRONRAG_CREDENTIAL_PREVIOUS_MASTER_KEYS=old-2025=<base64-key>,old-2026=<base64-key>
IRONRAG_AI_PROVIDER_API_KEYS_JSON_B64=<standard-base64-of-compact-JSON-object>
```

Credential'ы автоматически регистрируются при следующем рестарте, а
обнаруженные модели каталога становятся доступны в binding profiles
админ-UI. Ключи provider-kind сохраняются как есть (без case folding
и замены разделителей), поэтому новый провайдер в каталоге не требует
изменения кода.

Base64-конверт не даёт dotenv/Compose переинтерпретировать `$`, `#`, кавычки,
backslash'и или Unicode внутри credential. Строки сохраняются byte-for-byte,
включая ведущие и завершающие пробелы. Map ограничен 256 записями и 1 MiB
decoded JSON; каждый provider kind — не более 128 UTF-8 байт, каждый
credential — не более 64 KiB.

Эта схема заменяет прежние `IRONRAG_<PROVIDER>_API_KEY`. Перед апгрейдом
перенесите значения в JSON-map; startup и `install.sh` отклоняют
некорректные или удалённые имена вместо молчаливого alias.

`IRONRAG_CREDENTIAL_MASTER_KEY` обязателен для шифрования credential at rest.
Свежий `install.sh` генерирует его сам. Ротация описана в
[CREDENTIAL-ENCRYPTION.md](./CREDENTIAL-ENCRYPTION.md).

## Telemetry

Свежие установки ничего не экспортируют. Локальные structured logs остаются
доступны; OpenTelemetry включается только при `IRONRAG_OTEL_ENABLED=true` и
явном `OTEL_EXPORTER_OTLP_ENDPOINT` на collector под вашим контролем.
Стабильный `ironrag.deployment.id` появляется только после этого opt-in.
Экспорт логов отдельно выключен: в логах может быть содержимое документов и
запросов.
