<div align="center">

# Установка и начальная настройка

### Интерактивная и автоматическая установка, ключи провайдеров и телеметрия

[Обзор](./README.md) | [English](../en/INSTALL.md) | [AI bindings](./AI-BINDINGS.md) | [Шифрование credential](./CREDENTIAL-ENCRYPTION.md) | [Capacity planning](./CAPACITY-PLANNING.md)

</div>

## Интерактивная установка

Для существующей установки v0.6.3 выполните
[процедуру обновления до v0.6.4](#обновление-с-v063-до-v064) до запуска новых
образов или повторного запуска установщика.

```bash
curl -fsSL https://raw.githubusercontent.com/mlimarenko/IronRAG/master/install.sh | bash
```

Установщик проверяет ресурсы хоста, рекомендует профиль с ограничениями памяти,
CPU и параллелизма загрузки, затем запрашивает порт и начальную учётную запись
администратора. Перед записью он показывает итоговую конфигурацию. Ключи
провайдеров задаются переменными окружения ниже или в интерфейсе. При повторном
запуске сохраняются существующие секреты `.env` и выбранные ограничения, а
образы IronRAG обновляются до выбранного тега версии.

Текст на изображениях во всех профилях распознаёт LLM Vision через настроенный
binding `extract_text`. Xberg извлекает native text/structure и рендерит страницы;
ни один профиль не устанавливает локальную OCR-модель. Перед загрузкой изображений
или сканов настройте Vision-совместимый binding в **Admin → AI → Bindings**.

Установщик рекомендует `micro` при RAM меньше 8 GiB, `small` при RAM меньше
16 GiB, `medium` при RAM меньше 32 GiB, иначе `large`. Лимиты воркера:
768 MiB, 1536 MiB, 3072 MiB и 5120 MiB соответственно. Проверяется сумма caps
с резервом max(512 MiB, 12% RAM) для хоста. В Compose лимит воркера по умолчанию
3072 MiB; Helm запускает одну реплику с request 768 MiB и limit 3 GiB.
Native rendering и кодирование изображений по-прежнему требуют ограниченной RAM;
эти настройки не гарантируют ёмкость или качество распознавания любого корпуса.

Для существующих закреплённых лимитов примените `--recompute-resources`.
Иначе секреты и вручную заданные caps сохраняются. Образ backend собирает
`ironrag-extract` по locked Cargo graph; распознавание текста изображений
выполняет удалённый Vision-провайдер, а не встроенные модели.

Полный список флагов и переменных — `./install.sh --help`; `--plan-only`
печатает обнаруженный профиль без записи и деплоя.

## Переход встроенного PostgreSQL на версию 19

Новые установки со встроенной БД используют PostgreSQL 19 и pgvector 0.8.7.
Для существующей БД PostgreSQL 18 нужна миграция, а не замена образа. Сохраните
прежние том и образ. Не направляйте `PGDATA` PostgreSQL 19 на старые файлы и не
удаляйте том, чтобы обойти отказ при запуске.

1. Проверьте логический перенос на отдельном PostgreSQL 19. Сверьте кодировку,
   локаль, расширения, роли и права, журнал миграций схемы, векторные индексы и
   выборку записей. Измерьте длительность на копии базы. Хранилище контента,
   ключ шифрования и учетные данные БД не меняйте.
2. Остановите API, worker, коннекторы и другие процессы, которые пишут в базу,
   и дождитесь окончания записей. Сделайте и проверьте свежую резервную копию.
   Для базы используйте custom format `pg_dump`. Если нужны отдельные роли,
   сохраните их определения. Дамп ролей может содержать хеши паролей; храните
   его вместе с защищенной резервной копией. Прежний том не изменяйте.
3. Инициализируйте PostgreSQL 19 на **новом** томе и восстановите базу
   `pg_restore --exit-on-error`. В Compose сохраните имя проекта, укажите новый
   том через `IRONRAG_POSTGRES_VOLUME_NAME` и запустите только БД:
   `docker compose up -d --no-deps postgres`. В Helm подготовьте отдельный
   кластер на новом PVC вне Service, через который работает приложение. При
   переключении укажите этот PVC в `dependencies.postgres.persistence.existingClaim`.
   Не запускайте обычную выкладку приложения до завершения восстановления.
4. Проверьте версии сервера и расширения, журнал схемы, число записей, права
   и векторный поиск на восстановленной базе. Запустите приложение с новым
   томом и проверьте готовность до возврата трафика. Старый том остается
   точкой отката только до первых записей на новом; после них простое
   переключение назад потеряет новые данные.

Для внешнего PostgreSQL используйте процедуру обновления поставщика БД.
Проверьте pgvector до изменения подключения IronRAG.

## Обновление с v0.6.3 до v0.6.4

Для обновления нужны окно обслуживания и защищенный переход
[`text-publication-schema`](./CLI.md#migrate-text-publication-schema).
Точный predecessor, условия отказа и поведение схемы описаны в справочнике этой
команды. Не запускайте новый startup на старой схеме и не обходите отказ ручной
правкой SQLx ledger.

Подготовьте Compose-конфигурацию или Helm chart из опубликованного v0.6.4 и
проверенные digest образов backend, frontend и всех включенных комплектных
коннекторов. Миграция и новые startup, API и worker должны использовать **один
digest backend**. Исполняемый файл поставляется по пути
`/usr/local/bin/ironrag-maintenance`, это не `ironrag-cli`. Он читает окружение
backend, включая `IRONRAG_DATABASE_URL`.

Сохраните идентичность и настройки установки: проект Compose или release и
namespace Helm, базу и том/PVC PostgreSQL, расширение vector и его данные, том
или bucket исходных файлов, Secrets и `IRONRAG_CREDENTIAL_MASTER_KEY`.
Не создавайте новые ключи и хранилища, не обновляйте PostgreSQL, Redis или
расширение vector в рамках этой процедуры. При переходе на конфигурацию релиза
сохраните текущие образы зависимостей, несущих данные.

До dry-run и apply закройте прием новых запросов и данных коннекторов, дождитесь
завершения текущей работы и остановите **всех** старых writers: API, workers и
команды обслуживания. Учтите отдельные коннекторы, задания по расписанию и
другие реплики. Исключите их повторный запуск supervisor, autoscaler или
GitOps-контроллером. Одна команда остановки не доказывает дренирование. Убедитесь,
что writers больше не работают, затем сделайте и проверьте резервную копию
PostgreSQL и исходных файлов для отката. Сохраните соответствующие ключи,
конфигурацию и образы v0.6.3. Зависимости должны оставаться доступны для команды
обслуживания без запуска старого приложения.

### Compose

Используйте каталог существующей установки, прежнее имя проекта и `.env`.
Измените только образы приложения; вместо placeholders укажите проверенные
digest v0.6.4:

```dotenv
IRONRAG_BACKEND_IMAGE=pipingspace/ironrag-backend:v0.6.4@sha256:<backend-digest>
IRONRAG_FRONTEND_IMAGE=pipingspace/ironrag-frontend:v0.6.4@sha256:<frontend-digest>
```

В [Compose-конфигурации](../../docker-compose.yml) `startup`, `backend` и `worker`
используют общий образ backend и runtime-окружение. Проверьте конфигурацию и
загрузите образы без запуска сервисов:

```bash
docker compose config --quiet
docker compose pull startup backend worker frontend
```

После закрытия приема и завершения текущей работы остановите роли приложения.
Внешних writers остановите отдельно; перед продолжением выполните описанную
выше проверку резервной копии:

```bash
docker compose stop frontend backend worker startup
```

Запустите предварительную проверку из нового backend с окружением и mounts этого
сервиса. `--no-deps` запрещает Compose запускать обычный startup и зависимости;
`--entrypoint` выбирает поставляемый maintenance-бинарь:

```bash
docker compose run --rm --no-deps \
  --entrypoint /usr/local/bin/ironrag-maintenance backend \
  migrate text-publication-schema --json
```

Проверьте успешный результат dry-run и резервную копию до отдельного шага apply.
Writers должны оставаться остановленными, образ и конфигурация не меняются:

```bash
docker compose run --rm --no-deps \
  --entrypoint /usr/local/bin/ironrag-maintenance backend \
  migrate text-publication-schema --apply --json
```

Только после успешного apply пересоздайте штатный контейнер startup:

```bash
docker compose up -d --no-deps --no-build --force-recreate startup
docker compose logs --tail=80 startup
docker compose ps -a startup
```

Дождитесь завершения `startup` с кодом 0. Наличия логов или работающего контейнера
недостаточно. Затем замените API, worker и frontend:

```bash
docker compose up -d --no-deps --no-build backend worker frontend
docker compose ps -a
```

Проверьте readiness API и worker и здоровье frontend. До открытия приема
замените образы всех включенных коннекторов проверенными артефактами v0.6.4,
сохранив их настройки. Эти команды не заменяют сервисы хранения и не удаляют тома.

### Helm

Используйте прежние имя release и namespace и проверенный values-файл для chart
из опубликованного v0.6.4. Сохраните образы зависимостей, Secrets, PVC и настройки
хранения. В `api.image.digest` и `worker.image.digest` задайте один проверенный
backend `sha256:…`, а в `web.image.digest` - проверенный digest frontend.
Сохраните соответствующие repositories. Digest имеет приоритет над `image.tag`:
смена тега при оставленном старом digest не заменит образ.

Штатный [startup Job](../../charts/ironrag/templates/startup-job.yaml) использует
`api.image`; init containers API и worker ждут Job своей ревизии release.
В chart **нет** value для команды миграции. Startup gate не останавливает старые
pods: API использует rolling update. Обычный `helm upgrade` не заменяет
дренирование writers и ручной переход схемы.

1. Закройте прием и приостановите GitOps reconciliation. Исключите восстановление
   старого числа реплик через HPA или других владельцев, затем уменьшите число
   реплик старых API и worker deployments до нуля, используя фактические имена
   ресурсов установки. Остановите остальных writers и убедитесь, что все старые
   writer pods и jobs завершены. Проверьте резервную копию.
2. Подготовьте отдельный одноразовый maintenance Job. Для получения конфигурации
   pod локально отрендерите только startup Job, а не Secrets. Здесь `RELEASE`,
   `NAMESPACE` и `VALUES` задают существующую установку и проверенные values нового
   релиза; команду выполняйте из checkout опубликованного v0.6.4:

   ```bash
   helm template "$RELEASE" ./charts/ironrag \
     --namespace "$NAMESPACE" -f "$VALUES" \
     --show-only templates/startup-job.yaml > maintenance-preview.yaml
   ```

   Нужен режим `startup.mode=startup_job`. Задайте Job отдельное имя вне имен chart
   `*-startup-r<revision>` и удалите Helm/Argo hook annotations, если они есть.
   Установите `spec.backoffLimit: 0` и `restartPolicy: Never` для pod.
   Сохраните ссылки на окружение, security context, mounts, pull secrets,
   ожидание зависимостей и метки для network policy. Проверьте, что указанные
   ConfigMap/Secret уже существуют и выбирают нужную БД, backend закреплен
   проверенным digest, а service account и network policy допускают требуемый
   доступ. На этом шаге не применяйте весь отрендеренный release.
3. Переопределите команду и аргументы контейнера `startup`:

   ```yaml
   command: ["/usr/local/bin/ironrag-maintenance"]
   args: ["migrate", "text-publication-schema", "--json"]
   ```

   Создайте проверенный Job в прежнем namespace. Дождитесь успешного завершения
   и проверьте отчет. Для apply создайте второй Job с другим именем, тем же образом
   и конфигурацией pod, добавив `"--apply"` в `args`. Требуется успешное
   завершение: ошибка или таймаут Job не разрешают запуск приложения.
4. После apply обновите существующий release с проверенными values и штатным
   числом реплик. Прием остается закрыт до завершения нового startup Job и
   готовности API, worker и web. Не используйте автоматический откат на старое
   приложение после перехода схемы:

   ```bash
   helm upgrade "$RELEASE" ./charts/ironrag \
     --namespace "$NAMESPACE" -f "$VALUES" \
     --wait --wait-for-jobs --timeout 20m
   ```

   До возобновления приема и reconciliation замените образы отдельно развернутых
   коннекторов проверенными образами того же релиза.

При отказе миграции или startup оставьте writers остановленными и разберите
причину из отчета. После apply возврат только старых образов не откатывает схему:
старый бинарь отклонит новый checksum. Для возврата к v0.6.3 восстановите
согласованную резервную копию с соответствующим ей runtime. Переход не запускает
re-ingest, не возвращает исторические head и не включает финансовые допуски или
semantic rerank.

## Обновление с v0.6.2 до v0.6.3

Сохраните существующий проект Compose, `.env`, том PostgreSQL, том или bucket
хранилища файлов и `IRONRAG_CREDENTIAL_MASTER_KEY`. Перед обновлением сделайте
резервную копию. Не пересоздавайте тома и ключ шифрования.

В существующем `.env` измените только образы приложения:

```dotenv
IRONRAG_BACKEND_IMAGE=pipingspace/ironrag-backend:v0.6.3
IRONRAG_FRONTEND_IMAGE=pipingspace/ironrag-frontend:v0.6.3
```

Из каталога этой установки загрузите образы и замените сервисы приложения:

```bash
docker compose pull startup backend worker frontend
docker compose up -d startup backend worker frontend
docker compose ps -a
docker compose logs --tail=80 startup
```

Образ backend общий для `startup`, `backend` и `worker`. Compose запускает новый
контейнер startup перед API и worker. Для исправной установки публичного v0.6.2
SQL-миграция не требуется: в v0.6.3 те же baseline и checksum. Startup
автоматически проверяет существующую схему и завершается без изменения журнала
миграций. Некорректный журнал или отсутствие объектов схемы останавливают запуск,
а не исправляются скрыто.

Убедитесь, что `startup` завершился с кодом 0, а сервисы приложения имеют статус
healthy. Эти команды не загружают и не заменяют PostgreSQL, Redis или объектное
хранилище. Обновление major-версии PostgreSQL или расширения vector в эту
процедуру не входит.

Отдельно развернутые комплектные коннекторы переведите на теги образов `v0.6.3`,
сохранив их настройки. В Helm обновите backend-образы API и worker вместе с
образом web, сохраните существующие Secrets и хранилища и дождитесь штатного
startup job перед запуском приложения.

Собственные REST-клиенты полного графа должны заменить
`GET /v1/knowledge/libraries/{libraryId}/graph` на подготовку графа, описанную в
[release notes](../../CHANGELOG.md#063). Встроенный frontend уже использует ее.

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

Телеметрия браузера (Core Web Vitals, ошибки JavaScript, отказы API, release
health и продуктовые события) тоже выключена по умолчанию. Она встраивается в
образ фронтенда через `VITE_OTEL_EXPORTER_OTLP_ENDPOINT` (базовый URL; приложение
добавляет `/v1/traces`, `/v1/metrics` и `/v1/logs`) и остаётся выключенной, пока
пользователь не даст согласие в интерфейсе. Same-origin путь `/otlp/`
проксируется web-контейнером на `IRONRAG_OTLP_UPSTREAM`. Значения
редактируются в браузере: содержимое документов, вопросы, логины, email,
телефоны и query-параметры URL не экспортируются, а идентификатор сессии
случайный для каждой вкладки.
