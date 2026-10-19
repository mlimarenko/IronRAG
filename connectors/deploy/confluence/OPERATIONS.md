# Confluence connector — operational notes

Deployment quick-start, `.env`/`routing.yaml` setup — see [`docker-compose.yml`](docker-compose.yml)
here and the [connectors README](../../README.md#running-one).

## Права state-volume

State-volume должен быть доступен для записи uid/gid `10001`. Процедура — в
[`connectors/README.md`](../../README.md#state-volume-permissions).
Для volume этого коннектора: `docker run --rm -v
<volume>:/state alpine chown -R 10001:10001 /state`. Ошибка прав проявляется как
`sqlite3.OperationalError: attempt to write a readonly
database` и restart-loop.

## Аутентификация для API-smoke

`POST /v1/iam/sessions` с учётными данными пользователя выдаёт cookie
`ironrag_ui_session`. Передавайте её при ручных smoke-проверках API.

## Как узнать текущий тег образа

Не фиксированное значение — см. [connectors README](../../README.md#running-one):
`GET /health` отвечает именем источника и версией сборки, которая реально
запущена; тег образа — это `IRONRAG_VERSION`, под которым был выпущен
используемый релиз IronRAG.
