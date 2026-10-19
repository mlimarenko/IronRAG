# Конвейер данных IronRAG

## Точки входа

Контент поступает через:

- `POST /v1/content/libraries/{libraryId}/documents` для JSON с текстом и
  загрузки файла через `multipart/form-data`
- `POST /v1/content/documents/{documentId}/revisions` для добавления или
  замены текста и замены файла через `multipart/form-data`
- `POST /v1/content/web-runs` для одной страницы или рекурсивного обхода сайта

Вопросы поступают через `POST /v1/query/sessions/{sessionId}/turns`.
Веб-интерфейс и MCP используют одинаковые сохранённые данные и службу поиска.

## Приём и поиск

IronRAG нормализует документы перед разбиением на фрагменты, построением
векторов, извлечением графа и поиском. Поддерживаются Markdown, текст,
исходный код, JSON, YAML, JSONL/NDJSON, TOML, PDF, изображения, DOCX, PPTX,
CSV, TSV, XLS, XLSX, XLSB, ODS и веб-страницы. Видео не поддерживается.

Новые библиотеки используют значение
`IRONRAG_RECOGNITION_DEFAULT_RASTER_IMAGE_ENGINE`: `docling` или `vision`;
значение по умолчанию — `vision`. Политику библиотеки меняет
`PUT /v1/catalog/libraries/{libraryId}/recognition-policy`. Если для `vision`
не настроена совместимая привязка модели, приём завершается ошибкой.

PDF, DOCX и PPTX обрабатываются Docling на CPU. Для PDF доступны:

- `IRONRAG_DOCLING_PAGE_BATCH_SIZE`
- `IRONRAG_DOCLING_PAGE_STREAM_WINDOW_PAGES` (по умолчанию `40`)
- `IRONRAG_DOCLING_MAX_CONCURRENCY`
- `IRONRAG_INGESTION_HEAVY_PIPELINE_PARALLELISM=auto`

Параметры ограничивают параллельную обработку больших PDF. При нехватке памяти
документ завершается с ошибкой `docling_insufficient_memory`.

PostgreSQL хранит пользователей, рабочие пространства, библиотеки, документы,
данные поиска, события аудита и векторы pgvector. Байты исходников хранятся в
настроенном хранилище. Граф знаний содержит типизированные сущности и связи из
проиндексированного контента.

Деградация графа не превращает приём читаемого контента в отказ задания.
Попытка остаётся `succeeded`, задание — `completed`; ответ
`GET /v1/ingest/attempts/{attempt_id}` содержит нефатальный код
`failure_code: graph_degraded` в попытке. Для обнаружения этого исхода
не нужно читать `details_json` стадий. Готовый граф очищает этот код.
Транзиентная ошибка записи owner-record графа допускает один повтор;
подтверждённый пустой 404 провайдера — тоже не более одного повтора на вызов.
Непустой или непрочитанный 404 не получает этот повтор.

## Конфигурация поиска

У библиотеки есть `retrieval_config`, доступный через
`GET /v1/catalog/libraries/{id}/retrieval-config` и
`PUT /v1/catalog/libraries/{id}/retrieval-config`; для записи нужны права
администратора.

| Путь ключа | Тип | По умолчанию | Действие |
|---|---|---|---|
| `lexical.textSearchConfig` | string | `"simple"` | Конфигурация полнотекстового поиска PostgreSQL. Имя должно существовать в `pg_ts_config`; иначе API вернёт HTTP 400. |

Например, `"english"` включает английский стемминг, который может улучшить
поиск морфологически близких форм за счёт точного совпадения.

Ответы объединяют лексический, векторный и графовый поиск с цитатами. Если
материалов недостаточно, IronRAG сообщает об этом, а не выдаёт неподтверждённый
ответ за обоснованный.

## Видимость источника

Ответ версии объявляет `contentVisibilityContract: "visibility-intent/1"`.
Клиент должен проверить эту capability до отправки работы, зависящей от видимости.
Её отсутствие не разрешает использовать старую writable policy.

`POST /v1/content/documents/{documentId}/visibility-intents` принимает отдельное
обязательство скрыть источник, даже если обработка payload занята. Запрос содержит
`idempotencyKey`, `sourceIdentity`, явно переданный nullable `materializationToken`
и полный descriptor `replacement`: `checksum`, `byteSize`, `mimeType`, `fileName`,
`title`, `languageCode`, `sourceUri`, `documentHint`. Nullable metadata описывает
эту замену, а не наследует поля предыдущей revision. Повторная отправка использует
тот же ключ и identity.

Приём очищает публичный head и фиксирует retained revision floor. Matching
replacement передаёт `visibilityIntentId` из receipt; первый bound payload key
остаётся неизменным. Успех unrelated или superseded работы не снимает обязательство.
Текущим становится только точный bound READY item выше floor. Старые writable поля
`visibilityPolicy` и `visibility_policy` отвергаются.

Публичные source, revision, chunk, metadata, retrieval и cached-answer reads
используют `content_current_revision`. Revision-list endpoint показывает текущую
опубликованную revision, а не скрытую историю. Имя исходного файла берётся из
immutable descriptor опубликованного item, не из изменяемой метки документа.
Protected metadata меняется через replacement admission, без перезаписи принятого
descriptor. Append требует явного текущего `baseRevisionId`.

DELETE принимает `Idempotency-Key`. Использование ключа для другого документа —
конфликт; точный повтор возвращает исходные mutation и operation. Удаление очищает
публичные pointers, сохраняя base принятого item для аудита.

## Резервное копирование и восстановление библиотеки

Экспорт:

```
GET /v1/content/libraries/{libraryId}/snapshot?include=library_data,blobs
```

Ответ — сжатый `.tar.zst` с `manifest.json`, данными PostgreSQL и, при выборе
`blobs`, исходными файлами.

Импорт:

```
POST /v1/content/libraries/{libraryId}/snapshot?overwrite=reject|replace
Content-Type: application/zstd
Body: raw .tar.zst архив
```

Импорт возвращает `202 Accepted` и `operationId`. Дождитесь терминального
статуса `ready` или `failed` через
`GET /v1/ops/operations/{operationId}`. В `resultJson` успешной операции есть
`postgresRowsByTable` и `blobsRestored`; при ошибке указан `failureCode`.

Архив можно восстановить в пустую библиотеку или обратно в исходную. Его нельзя
восстановить во вторую библиотеку, пока исходная библиотека существует в том же
развёртывании: операция завершается ошибкой `snapshot_source_already_present`.

Схема архива версии 3 сохраняет счётчик ревизий, текущий visibility intent и его
floor, неизменяемую идентичность запроса и fingerprint, привязку payload к intent
и точного владельца опубликованной ревизии — mutation item. Полный inventory и
замкнутость ссылок проверяются до записи в целевую библиотеку. Отсутствующая
идентичность, ссылки в чужую область, неоднозначная lineage intent, неверное
соотношение counter/floor и поддельный readable owner приводят к отказу.
Владельца, идентичность и решение о видимости не восстанавливают из истории.
Удалённые `visibility_policy` и `visibilityPolicy` в схему 3 не входят и
отвергаются при проверке архива.

Backup принадлежит источнику, а не публичной проекции: при выборе `blobs` архив
включает также ограниченные исторические и ещё обрабатываемые исходные байты.
Запрет сохраняется после restore и ремонта пустых head; публичной становится
только точная привязанная READY-ревизия. Chunk-векторы сохраняют current-only
жизненный цикл, счётчики отражают фактические строки. Архивы библиотек и workspace
поддерживают только схему 3; версии 1 и 2 отвергаются с
`snapshot_unsupported_schema_version`, без преобразования или fallback.
Выгрузите библиотеку из развёртывания этой версии заново или переиндексируйте источники.
Экспорт библиотек и workspace требует существующих полномочий системного
администратора; document/library read и workspace admin не дают доступа к истории.
