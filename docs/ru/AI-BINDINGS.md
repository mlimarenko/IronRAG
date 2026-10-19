# AI bindings (привязки моделей)

_AI binding profile_ — это связка одной понятной оператору
задачи (понять запрос, посчитать векторы, сформировать ответ, …) с
`ai_account` и catalog model. Prompt и sampling settings хранятся inline в этом
binding. Все задачи LLM в IronRAG используют выбранный профиль привязки.

В UI ровно пять обязательных профилей и три опциональных — `extract_text`,
`query_rerank` и `graph_entity_type`. Runtime-стадии эмбеддинга запроса и
визуального анализа напрямую используют назначенный профиль и не являются
отдельными настраиваемыми привязками. Исключения — semantic rerank и
типизация сущностей графа: каждая стадия резолвит собственную привязку
(`query_rerank`, `graph_entity_type`). Semantic rerank без привязки
деградирует к детерминированному lexical fallback; ingest резолвит
`graph_entity_type` после детекции кандидатов и до вычисления identity и
рёбер.

## 1. Модель данных

Строка в `ai_binding` хранит следующие части:

| Поле                         | Источник                  | Назначение                                                           |
| ---------------------------- | ------------------------- | -------------------------------------------------------------------- |
| `binding_purpose`            | enum `ai_binding_purpose` | один из внутренних runtime-purpose'ов ниже                           |
| `scope_kind`                 | enum `ai_scope_kind`      | `instance` / `workspace` / `library`                                 |
| `workspace_id`, `library_id` | nullable                  | заполняется согласно `scope_kind`                                    |
| `account_id`                 | `ai_account`              | API credential + base URL + provider catalog                         |
| `model_catalog_id`           | `ai_model_catalog`        | типизированная модель и её capabilities                              |
| prompt/sampling-поля         | внутри `ai_binding`       | system prompt, temperature, top_p, output override, extra parameters |
| `binding_state`              | enum `ai_binding_state`   | только `active`, `invalid` или `disabled`                            |

Runtime резолвит **эффективную** привязку, обходя scope-лестницу:

1. library-scoped active binding для `(library_id, purpose)`
2. workspace-scoped active binding для `(workspace_id, purpose)`
3. instance-scoped active binding только для `(purpose)`

Первое совпадение выигрывает. Если назначенный профиль не настроен, стадия
падает громко. Случайная модель или provider не подставляются.

## 2. Профили в UI

Настройте профили в UI:

| Профиль                   | Обязательный | Binding purpose     | Контракт                                                                                                                                    |
| ------------------------- | -----------: | ------------------- | ------------------------------------------------------------------------------------------------------------------------------------------- |
| Извлечение графа          |           да | `extract_graph`     | строгий JSON типизированных сущностей и связей                                                                                              |
| Векторные представления   |           да | `embed_chunk`       | одна embedding-модель для сегментов индекса и векторов запроса                                                                              |
| Понимание запроса         |           да | `query_compile`     | типизированный `QueryIR`, HyDE-развёртывание и консолидация запросов                                                                        |
| Semantic rerank           |          нет | `query_rerank`      | provider-backed semantic reranking отобранных кандидатов, привязка отдельно от компиляции запроса                                           |
| Типизация сущностей графа |          нет | `graph_entity_type` | типизированная классификация извлечённых сущностей до вычисления identity и рёбер; обслуживается эндпоинтом решений или быстрой чат-моделью |
| Генерация ответов         |           да | `query_answer`      | grounded-ответ с цитатами                                                                                                                   |
| AI-ассистент              |           да | `agent`             | tool-calling host для UI и MCP                                                                                                              |
| Понимание документов      |          нет | `extract_text`      | сложный текст/OCR/визуальный анализ после детерминированных парсеров                                                                        |

Эти восемь binding purpose'ов — весь binding enum. Runtime-события и
billing сохраняют собственную типизированную идентичность стадии, не создавая
лишних профилей моделей:

| Внутренний purpose  | Стадия-потребитель                                                                      | Контракт резолвинга                                                                                                                                  |
| ------------------- | --------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------- |
| `extract_text`      | ingest: сложный текст/OCR и визуальный анализ                                           | назначенный мультимодальный профиль «Понимание документов»                                                                                           |
| `extract_graph`     | ingest: graph builder                                                                   | строгий JSON-tagging сущностей/связей по чанку                                                                                                       |
| `graph_entity_type` | ingest: типизация сущностей графа                                                       | типизированная классификация извлечённых сущностей в один закрытый тип; классифицированный тип образует ключ identity узла и материализованные рёбра |
| `embed_chunk`       | ingest и query: индексирование сегментов и эмбеддинг вопроса                            | один общий профиль и vector space                                                                                                                    |
| `query_compile`     | query: компиляция типизированного `QueryIR`, HyDE-развёртывание и консолидация запросов | один профиль «Понимание запроса»                                                                                                                     |
| `query_rerank`      | query: provider-backed semantic rerank отобранных кандидатов                            | один профиль «Semantic rerank»; отсутствие привязки деградирует к детерминированному lexical fallback, не к `query_compile`                          |
| `query_answer`      | query: grounded ответ по retrieved bundle                                               | citation-aware длинная генерация; instruction following                                                                                              |
| `agent`             | UI in-product agent + MCP host `grounded_answer`                                        | tool-calling chat-модель с хорошим instruction following                                                                                             |

Допуск модели определяется типизированными capability/modality в каталоге. Он никогда
не угадывается по provider/model name, суффиксу или ручному словарю.

Для `agent` действует более строгая комбинация: у модели должна быть явно
задана catalog-role `agent` и capability kind `chat`, а provider profile обязан
объявлять и `chat`, и `tools` как `supported`. Свежий baseline создаёт
Agent binding из `query_answer` только при таком типизированном контракте
provider; явно настроенный Agent binding остаётся без изменений.

Если provider не возвращает typed metadata в model-list, генератор каталога
принимает operator manifest через `IRONRAG_AI_MODEL_CAPABILITIES_JSON_B64`. После
base64-декодирования это object с provider kind на первом уровне и точной
model identity на втором. Каждая запись содержит только `capabilityKind`
(`chat` или `embedding`) и `modalityKind` (`text` или `multimodal`). Манифесты
с дублями или превышением лимита отклоняются; записи с некорректной сигнатурой и
модели без typed declaration пропускаются.

### Провайдеры решений (TypeSafe)

Провайдер, чей profile объявляет `capabilities.decision: supported`, — это
типизированный эндпоинт решений (TypeSafe System One), а не чат-вендор. Он
обязан объявить все генеративные capability (`chat`, `embeddings`, `vision`,
`tools`, `streaming`) как `unsupported`, а все пути обнаружения моделей — как
`capabilityKind: decision`. Такой провайдер обслуживает только два purpose'а
решений, `graph_entity_type` и `query_rerank`; его модели могут разрешать
только их. Привязка на генеративный purpose отклоняется даже при
`disabled` в запросе.

Оба назначения решений выпущены: `query_rerank` с IRG-187 оценивает шортлист
через примитив SystemOne `Score`, а `graph_entity_type` с IRG-185
классифицирует извлечённые сущности через примитив `Choice` до вычисления
identity и рёбер. Активные привязки на любое из этих назначений принимаются.
Seed кладёт модель `jev-latest` провайдера TypeSafe Jev для просмотра. Обычный
чат-провайдер не затронут: быстрая чат-модель, чья строка каталога разрешает
`graph_entity_type` или `query_rerank`, остаётся привязываемой активной и
обслуживает ту же стадию.

Аккаунт TypeSafe проверяется по OpenAI-образному листингу `GET /v1/models`
(`credentials.validationMode: model_list` в его profile) — тем же путём, что и
у остальных seeded-провайдеров.

## 3. Wire-level структура prompt'а (это важно для кэширования)

OpenAI-совместимый запрос имеет такую форму:

```jsonc
{
  "model": "<model_name>",
  "messages": [
    { "role": "system", "content": "<binding.system_prompt ИЛИ встроенный prompt purpose'а>" },
    { "role": "user",   "content": "<purpose-specific user prompt>" }
  ],
  "temperature": ...,
  "top_p": ...,
  "response_format": { "type": "json_schema", "json_schema": { ... } },
  "max_completion_tokens": ...
}
```

Статичный system prompt **всегда** идёт первым сообщением, переменный
user — следом. Именно такой порядок ждёт автоматический OpenAI prompt
caching: самый длинный неизменный prefix лежит в начале сериализованного
тела запроса, и одинаковые prefix'ы хэшируются в один cache-key.

Следствия:

- JSON-схема `response_format` — часть тела запроса и часть cache-key.
  Изменил схему — инвалидировал все cached prefix на стороне провайдера.
- `temperature`, `top_p`, `max_completion_tokens`, резолвенное
  `model_name` — тоже в теле. Не дёргайте их per-call.
- `extra_parameters_json` из binding и typed provider request policy сливаются в тело
  как есть. **Никаких per-call динамических значений** (user id,
  request id, timestamp, …). Любая динамика убивает provider-side
  prompt cache и каждый вызов платит полную latency.

Биллинг нормализует cache-счётчики по протоколу провайдера. У
OpenAI-compatible cached tokens входят в общий input и вычитаются перед
созданием charge за обычный input. У Anthropic input, cache-creation и
cache-read — раздельные счётчики; вместе они используются только для выбора
ценового tier по размеру контекста. Cache-creation Anthropic тарифицируется
по cache-write input ставке модели, а cache-read — по её cached-input ставке.

## 4. Выбор модели по профилю

Реальные tradeoff'ы latency/качество/цена — provider-specific. Со
стороны IronRAG важна _форма_ работы:

### Понимание запроса (`query_compile`)

- Hot path на каждый grounded ответ; cold p95 answer-пайплайна именно
  на этом вызове, когда IR cache промазал.
- Input маленький (встроенный system prompt + вопрос ~< 1 КБ +
  JSON-schema; ~1.5–10K токенов в зависимости от prompt'а).
- Output — строгий JSON, обычно 200–500 токенов.
- System prompt и схема статичны — ожидается высокий cache-hit на
  стороне провайдера при условии, что в `extra_parameters_json` нет
  динамики.
- Оптимизируйте по **time-to-first-token**, а не по throughput;
  обычно «меньше, но с хорошим structured output» — правильный выбор.
- Качество критично: плохой IR ломает retrieval scope/focus. Всегда
  прогоняйте кандидата на стабильном наборе golden-вопросов перед
  сменой активной привязки.

### Semantic rerank (`query_rerank`)

Provider-backed semantic rerank резолвит привязку `query_rerank`, поэтому его
можно привязать, бюджетировать и переключить на быструю non-reasoning модель,
не трогая компилятор типизированных запросов. Алиаса на `query_compile` нет:
при отсутствии привязки `query_rerank` стадия деградирует к детерминированному
lexical fallback. Компиляция и rerank остаются отдельными runtime-вызовами со
своими схемами, deadline, лимитами конкурентности, трассировкой и accounting.

#### Rollout semantic rerank

Provider-backed semantic rerank включается явно и по умолчанию имеет режим
`IRONRAG_QUERY_SEMANTIC_RERANK_MODE=off`. Безопасный rollout:
`off` -> `shadow` -> `active`. Настройка `IRONRAG_QUERY_RERANK_ENABLED` —
master gate: startup отклоняет `shadow` или `active`, если она равна `false`.

- `off` продолжает использовать детерминированную lexical-эвристику с resolved
  standalone retrieval question и не делает вызов provider.
- `shadow` отправляет resolved standalone retrieval question и ограниченные
  фрагменты кандидатов в активный профиль «Semantic rerank» через одну ограниченную на
  процесс low-priority задачу только при настоящем result-cache miss. Порядок
  ответа не меняется, а query path не ждёт ответа provider. Обычные cache hit
  остаются быстрыми и не создают повторный shadow-вызов или billing-запись;
  историческое переиспользование явно фиксируют query-execution replay row и
  cache-hit log.
- `active` применяет только валидный provider ranking и ограничен отдельным
  turn-бюджетом ожидания. Настроенный timeout — это provider decision budget,
  который начинается до lookup binding; он не может превышать compile-time
  предел turn wait (1000 мс, `SEMANTIC_RERANK_TURN_WAIT_MS`), иначе active-режим
  не вызвал бы provider никогда: такая конфигурация отвергается на старте (fail
  closed, сообщение `query_semantic_rerank_timeout_ms`). Дефолт timeout равен
  этому же пределу. Ответный ход не ждёт дольше turn-бюджета; если provider
  budget не помещается в него, вызов provider не начинается, и сразу
  используется детерминированная lexical fallback-эвристика; если вызов начат и
  turn-бюджет истёк, fallback тот же. Каждая деградация пишется структурным
  warning `semantic rerank degraded to the deterministic lexical fallback` с
  полем `reason` и нарастающим `degradations_total`, а также инкрементирует
  экспортируемый счётчик `ironrag.semantic_rerank.degradations` с тем же
  bounded-лейблом `reason` (`provider_budget_exceeds_turn_wait`,
  `turn_wait_exceeded`, `missing_binding`, `provider_failure`, `invalid_response`,
  `timed_out`), поэтому постоянный fallback виден оператору без разбора логов.
  Lookup binding и durable reservation не отменяются посреди операции с БД, но
  расходуют provider budget; provider получает только оставшееся время. Если
  budget закончился после reservation, известная запись terminalize'ится, а
  provider не вызывается. При отсутствии binding, timeout, ошибке provider,
  некорректном JSON или ошибке accounting используется та же детерминированная
  lexical fallback-эвристика. Обязательный completion/accounting после ответа
  provider может добавить DB overhead уже за пределами decision deadline.

Provider получает текст кандидатов и непрозрачные числовые индексы, но не
внутренние UUID IronRAG entity, relationship, chunk, document, workspace или
library.
Runtime принимает ровно один конечный score в диапазоне `[0, 1]` на каждый
отправленный index; дубли, пропуски, лишние поля и значения вне диапазона
отклоняются. Число кандидатов, raw-символы одного кандидата и общий объём raw
query+candidate текста ограничены пятью настройками
`IRONRAG_QUERY_SEMANTIC_RERANK_*` и compile-time пределами (32 кандидата, 2400
raw-символов на кандидата, 32000 raw-символов всего). После UTF-8 encoding и
JSON escaping полное user message имеет отдельный жёсткий предел 96 КиБ;
кандидаты удаляются с хвоста, пока payload не уложится. Provider scores
управляют только порядком; исходные retrieval scores остаются неизменными.

### `query_answer`

- Длинная генерация с inline citations. Output доминирует в latency.
- Streaming помогает; UI assistant и MCP grounded-answer tool оба
  потребляют streamed-деltas.
- Более качественная модель обычно отбивает разницу — это видимый
  пользователю продукт.

### Векторные представления (один профиль для индекса и запросов)

- Это одна операторская привязка, а не две независимо выбираемые модели.
  `embed_chunk` индексирует чанки, а query-путь резолвит этот же профиль для
  вопроса. Второго purpose для query embedding нет.
- Каждый сохранённый вектор и query lookup используют secret-free ключ
  execution-профиля `embedding-profile:v1:<sha256>`. Он учитывает resolved
  provider/model path и нормализованные request parameters, но не scope, binding
  row, requested purpose и значение секрета. Поиск требует точного совпадения
  ключа и одной однозначной размерности.
- `extraParametersJson.dimensions` имеет приоритет над
  `metadataJson.dimensions` model catalog. Оба значения при resolution
  проверяются как положительные целые числа, допустимые для storage; некорректное
  явное значение завершается fail closed без fallback. Catalog-only размерность
  входит в profile key, но не добавляется в upstream request body.
- Одинаковая размерность **не** делает два embedding space совместимыми. Любая
  смена профиля, изменившая ключ, требует
  `ironrag-maintenance rebuild vector-plane --source-library <uuid>`, даже если
   число значений не изменилось.
- Если у библиотеки есть активный source material, но нет векторов точного
  активного профиля, retrieval возвращает явное требование rebuild, а не тихий
  lexical-only или пустой vector result.
- Действительно пустая библиотека — корректное типизированное состояние.
  Query preflight не вызывает embedding provider и ANN lanes и не требует
  невозможного rebuild.
- Rebuild потоково читает сохранённые chunks через keyset cursor, пишет
  векторы provider batches и пересчитывает каждый vector manifest один раз в
  конце. Вся библиотека не удерживается в памяти, lane не пересчитывается после
  каждой строки.
- На ingest-масштабе важен throughput, на query-пути — latency. Это два замера
  одной модели, а не причина разрешать несовместимые vector space.

### `extract_graph`

- Строгий JSON output per chunk; запускается многократно на документ.
  Cost и throughput доминируют над single-call latency.
- Маленькая быстрая модель со structured output обычно ок.

### Понимание документов (`extract_text`)

- Где этого достаточно, сначала работают детерминированные файловые парсеры и
  OCR. Назначенная мультимодальная модель понимания документов берёт сложное
  извлечение и визуальный контент, где нужно model reasoning.
- Визуальный анализ использует эту же модель. Привязка принимается, только если
  каталог объявляет chat-capable мультимодальную модель, а provider поддерживает
  визуальный input. Без подходящего профиля остаётся детерминированное
  извлечение, а model-only анализ недоступен.

### `agent`

- Tool-calling chat-модель: UI in-product agent и MCP host для
  `grounded_answer`. Должна аккуратно следовать tool-call схемам и
  выбирать правильный tool с правильными аргументами.

### Режим рассуждения (на привязку)

Привязка может нести типизированный выбор режима рассуждения рядом с сырым
`extraParametersJson`. Он хранится под каноническим ключом
`extraParametersJson.reasoningMode` как `{"version": 1, "mode": "none"}`;
отсутствие ключа — честное состояние **не задано**. Административный UI
редактирует его селектором **Режим рассуждения**, который показывается для
всех purpose, кроме Embeddings. Этим ключом владеет селектор, и он никогда не
редактируется через JSON дополнительных параметров — у типизированного выбора
один автор.

**«Не задано» — это не «Отключено» и не исходное значение провайдера.**
Оставляя селектор на _По умолчанию (политика провайдера)_, вы отправляете
привязку с прежней автоматической политикой и сырыми параметрами: `ProviderReasoningPolicy` провайдера
(`Forward` или `DisableWithNone`) и все сырые параметры продолжают действовать
точно так же. UI не обещает для этого состояния никакого конкретного значения
провайдера.

Выбрать **Отключено** можно только для той самой пары провайдер/модель, чья
запись каталога модели подтверждает это в `metadataJson.reasoningCapability`:

- `mode` — `none`;
- `wire` — один из документированных wire-контролов `reasoning_effort_none`
  или `thinking_disabled`;
- `evidence` — непустой и называет первоисточник, подтвердивший именно эту
  пару.

Ничего не выводится из профиля провайдера, его способности `chat`, имени,
семейства или суффикса модели. Служебная серверная проекция
(`_providerModelReasoning`) или клиентская request policy никогда не даруют
способность. Неподтверждённые или нечитаемые метаданные объясняются
read-only, и режим выбрать нельзя. Сохранённый выбор, который не читается
(чужая версия или тип, неизвестный режим) или не подтверждён для привязанной
модели, показывается честно, и снять его может только явный **Сбросить** —
он никогда не переписывается в `none`. Смена аккаунта или модели отменяет
предыдущее решение, даже если новая пара тоже подтверждает отключение.
Сохранение заблокировано до явного **Подтвердить для этой пары** или
**Сбросить**; для неподтверждённой пары доступен только сброс. Возврат к
исходной модели не восстанавливает прежнее решение. При блокировке раздел
**Модель и параметры** открывается автоматически и показывает причину и
действие для её устранения.

Когда выбрано **Отключено**, сервер отправляет ровно один wire-контрол и
очищает другой принадлежащий ему переключатель до выхода запроса, поэтому два
контрола рассуждения никогда не уходят вместе:

| Подтверждённый wire     | Отправляемый параметр провайдера |
| ----------------------- | -------------------------------- |
| `reasoning_effort_none` | `reasoning_effort: "none"`       |
| `thinking_disabled`     | `thinking: {"type": "disabled"}` |

Объявленный контрол может присутствовать в сыром JSON только со своим точным
значением; другой принадлежащий переключатель — конфликт, даже если он тоже
отключает рассуждение. Диалог называет конфликтующий параметр и по запросу
удаляет только этот ключ, не теряя несвязанный JSON. `reasoningMode`,
вписанный в сырой JSON, отклоняется, пока не будет удалён.

Словарь сейчас закрыт значением `none`, и первоисточниками подтверждены
только эти два wire-контрола:

- `reasoning_effort`: OpenAI документирует `none` (среди прочих уровней) для
  Chat Completions — <https://developers.openai.com/api/docs/guides/reasoning>.
- `thinking` и `reasoning_effort`: справочник Chat Completions DeepSeek
  документирует `thinking.type` `enabled`/`disabled` и `reasoning_effort`
  `none`/`low`/`high`/`max` для `deepseek-flash` и `deepseek-v4-pro` —
  <https://api-docs.deepseek.com/api/create-chat-completion>.

Эта документация подтверждает словарь wire, а не приём какой-либо развёрнутой
парой. Оператор по-прежнему подтверждает каждую конкретную модель в её записи
каталога; модели, которые сеет эта сборка, до этого остаются
неподтверждёнными.

## 5. Инспекция активных привязок

Привязки живут в Postgres. Этот запрос соединяет binding, account, provider и model
catalog и показывает каждый активный физический purpose:

```sql
SELECT
  b.scope_kind,
  b.workspace_id,
  b.library_id,
  b.binding_purpose,
  p.provider_kind,
  m.model_name,
  octet_length(coalesce(b.system_prompt,'')) AS sys_prompt_bytes,
  b.temperature,
  b.top_p,
  b.max_output_tokens_override
FROM ai_binding b
JOIN ai_account a          ON a.id = b.account_id
JOIN ai_provider_catalog p ON p.id = a.provider_catalog_id
JOIN ai_model_catalog m    ON m.id = b.model_catalog_id
WHERE b.binding_state = 'active'
ORDER BY b.binding_purpose, b.scope_kind;
```

Эффективная привязка для одной library + одного purpose'а (повторяет
runtime-резолвер):

```sql
WITH library AS (
  SELECT id AS library_id, workspace_id FROM catalog_library WHERE id = $1
)
SELECT b.scope_kind, p.provider_kind, m.model_name
FROM ai_binding b
CROSS JOIN library
JOIN ai_account a          ON a.id = b.account_id
JOIN ai_provider_catalog p ON p.id = a.provider_catalog_id
JOIN ai_model_catalog m    ON m.id = b.model_catalog_id
WHERE b.binding_state = 'active'
  AND b.binding_purpose = $2
  AND (
        (b.scope_kind = 'library'   AND b.library_id   = library.library_id)
     OR (b.scope_kind = 'workspace' AND b.workspace_id = library.workspace_id)
     OR (b.scope_kind = 'instance')
  )
ORDER BY CASE b.scope_kind
           WHEN 'library'   THEN 1
           WHEN 'workspace' THEN 2
           WHEN 'instance'  THEN 3
         END
LIMIT 1;
```

## 6. Типичные грабли

- **Нет активной привязки → громкий fail.** Стадии не запускаются с
  тихим default. Если `query_compile`-привязка не настроена,
  grounded-answer пайплайн возвращает `409/422` с
  `QueryCompile binding is not configured`. Чините привязку, не
  вводите fallback.
- **Embedding-space drift.** Любая смена active embedding execution profile,
  которая меняет его profile key, требует vector rebuild. Это относится и к
  моделям одинаковой размерности, и к переходам вроде `1024` → `3072`:
  совпадение dimension само по себе не доказывает совместимость координат.
- **Динамика в `extra_parameters_json`.** Всё, что меняется
  per-request (user id, timestamp, request id), попадает в тело и
  убивает provider-side prompt cache.
- **Правки system prompt.** Изменение встроенного
  `QUERY_COMPILER_SYSTEM_PROMPT` инвалидирует prompt cache у каждой
  привязки, которая использует встроенный prompt. Относитесь к этой
  константе как к части схемы и версионируйте изменения через
  `QUERY_IR_SCHEMA_VERSION` в IR cache key. Prompt'ы graph-extraction
  живут в источнике graph-сервиса и не имеют отдельной именованной
  compile-time константы.
- **Микс scope'ов.** Library-override полностью прячет workspace и
  instance fallback'и. Если поставили library-scoped
  `query_compile`-привязку, workspace и instance для этой библиотеки
  не используются. Используйте scope только когда реально нужен
  per-library / per-workspace override.
- **Операторский `max_output_tokens_override` сохраняется после
  рестартов.** Startup seed заполняет `max_output_tokens_override`
  только если у binding-строки это поле ещё не задано. Поднятый
  оператором output budget (например для graph extraction)
  сохраняется между рестартами backend'а и не откатывается молча к
  значению из catalog-дефолта.

## 7. Смена активной привязки

1. Создайте или выберите `ai_account` для provider.
2. Выберите модель. Диалог привязки показывает строки `ai_model_catalog` этого
   провайдера вместе с тем, что ключ аккаунта достаёт прямо сейчас: список
   читается у провайдера через
   `GET /v1/ai/accounts/{account_id}/available-models`. Если у выбранной модели
   ещё нет строки каталога, она создаётся при сохранении. OpenRouter и RouterAI
   публикуют модели с ценами за токен, поэтому seed засеивает из них
   отобранный платный набор — OpenRouter в долларах США, RouterAI в рублях.
   GPTunnel не публикует прайс без авторизации, поэтому он не засеивает ни
   одной модели, и его модели берутся из живого списка аккаунта.
3. Через UI администрирования или AI configuration API сохраните логический
   профиль на нужном scope. Модель, prompt, sampling, output override и extra parameters
   хранятся прямо в `ai_binding`; не пишите в таблицу вручную.

Созданная так строка несёт только то, что опубликовал провайдер: длину
контекста, потолок вывода, входные модальности и тарифы. Если тарифов
провайдер не публикует, строка запишет нули, и диалог предупредит об этом до
сохранения — проставьте реальные тарифы в каталоге, если хотите считать
расходы по этой модели. Из имени модели ничего не выводится.

Провайдер также объявляет, какие назначения не покрывает ни одна его
засеянная модель, и какого рода этот пробел. `provider_cannot` — это нехватка
возможности: у Anthropic нет эндпоинта эмбеддингов, поэтому под эмбеддинги
нужен второй провайдер. `operator_supplies` означает, что назначение
провайдер обслуживает, а модель выбираете вы из живого списка выше; в этой
группе GPTunnel, LiteLLM, Ollama и Qwen.

## 8. Провайдеры каталога

| Провайдер                 | Chat | Vision | Embedding | Заметки                                                                                                       |
| ------------------------- | ---- | ------ | --------- | ------------------------------------------------------------------------------------------------------------- |
| **OpenAI**                | ✅   | ✅     | ✅        | Прямой API                                                                                                    |
| **DeepSeek**              | ✅   | ✅     | —         | Очень низкая стоимость; V4.1-Flash читает изображения; без эмбеддингов                                        |
| **Qwen / DashScope-intl** | ✅   | ✅     | ✅        | Стоимость близка к DeepSeek; засеивает платные chat/vision; модель для эмбеддингов выбирает оператор          |
| **GPTunnel**              | ✅   | ✅     | ✅        | Router: не публикует прайс без авторизации, поэтому моделей не засеивает — добавьте их из аккаунта            |
| **OpenRouter**            | ✅   | ✅     | ✅        | Router: засеивает отобранный платный набор в USD                                                              |
| **RouterAI**              | ✅   | ✅     | ✅        | Router: засеивает тот же chat-набор в RUB                                                                     |
| **MiniMax**               | ✅   | ✅     | —         | Прямой API или Token Plan                                                                                     |
| **Ollama**                | ✅   | ✅     | ✅        | Полностью локально, air-gapped, GPU опционален — см. [OLLAMA.md](./OLLAMA.md)                                 |
| **LiteLLM**               | ✅   | ✅     | ✅        | Self-hosted OpenAI-compatible gateway: аккаунт OpenAI-compatible с base URL LiteLLM                           |
| **TypeSafe Jev**          | —    | —      | —         | Типизированный эндпоинт решений: только `graph_entity_type` / `query_rerank`, оба активны с IRG-187 / IRG-185 |

**Admin → AI → Bindings** показывает восемь purposes в runtime-порядке.
Настраивайте только те, что нужны включённым workflow. Scope: instance /
workspace / library.

Raster-image engine `vision` использует активный multimodal `extract_text`.
Переключите библиотеку на `docling`, чтобы OCR шёл локально без visual-model
вызовов. Выбор `vision` без совместимого `extract_text` падает громко.

Bootstrap ключей из `.env`: [INSTALL.md](./INSTALL.md).

## 9. Режимы рассуждения: аудит исходников и документации

Дата аудита: **2026-10-04**. Матрица охватывает 12 засеянных провайдеров из
[`catalog_seed.rs`](../../apps/api/src/app/catalog_seed.rs), проверенных на
`b3092cc3`; их определения не изменены на `17e553fe`. Здесь опубликован принятый
аудит IRG-166 rev4, а не проверка совместимости живого аккаунта и модели.

Код объявляет `requestPolicy.reasoning = disable_with_none` для OpenAI и
DeepSeek. Такая политика провайдера не доказывает, что любая его модель принимает
`none`. Другие OpenAI-compatible профили используют `forward`; Anthropic и Gemini
работают через native transports без смоделированного reasoning control.
`forward` не означает отсутствия такого контрола у вендора. TypeSafe в этой
конфигурации — decision endpoint, а не провайдер генеративного рассуждения.

| Провайдер | Политика каталога / реализация | Документированный контрол и первичный источник |
|---|---|---|
| OpenAI | `disable_with_none`; ограничения модели сохраняются | [`reasoning.effort` / `reasoning_effort`](https://developers.openai.com/api/docs/guides/reasoning); значения зависят от модели. GPT-6 Astra отвергает `none`; GPT-6.1 Sol не поддерживает `none`/`minimal`, по умолчанию `medium`. |
| Anthropic | Native Messages transport; контрол не смоделирован | [`output_config.effort`](https://platform.claude.com/docs/en/build-with-claude/effort): `low`, `medium`, `high`, `xhigh`, `max`, зависит от модели. `thinking.budget_tokens` — отдельный контрол. |
| Gemini | Native `generateContent`; контрол не смоделирован | [`generationConfig.thinkingConfig`](https://ai.google.dev/api/generate-content): целочисленный `thinkingBudget` или `thinkingLevel` (`THINKING_LEVEL_UNSPECIFIED`, `MINIMAL`, `LOW`, `MEDIUM`, `HIGH`). Поддержка уровня зависит от модели. |
| MiniMax | OpenAI-compatible `forward`; контрол не смоделирован | [`thinking.type` / `reasoning_effort`](https://platform.minimax.io/docs/api-reference/text-openai-api). Effort — только для M3.1-Flash-Preview, default `max`, `none` отвергается; M3.1-Flash и M2.x не позволяют выключить thinking. |
| DeepSeek | `disable_with_none`; endpoint control смоделирован | [`thinking.type` / `reasoning_effort`](https://api-docs.deepseek.com/api/create-chat-completion): `enabled`/`disabled` либо `none`, `low`, `high`, `max`; effort по умолчанию `high`. |
| OpenRouter | OpenAI-compatible `forward`; контрол не смоделирован | [`reasoning`](https://openrouter.ai/docs/guides/best-practices/reasoning-tokens) нормализует effort/token budgets. Нужна metadata конкретной модели; `mandatory=true` отвергает `none`, dynamic routers опускают поле. |
| RouterAI | OpenAI-compatible `forward`; не подтверждено | [Сайт вендора](https://routerai.ru/); первичная документация reasoning control не получена. OpenAI compatibility не доказывает поддержку моделью. |
| GPTunnel | OpenAI-compatible `forward`; не подтверждено | [Сайт вендора](https://gptunnel.ru/); JS-shell не предоставил первичного содержимого о reasoning control. Модели не засеяны. |
| Ollama | OpenAI-compatible `forward`; контрол не смоделирован | [`reasoning_effort` / `reasoning.effort`](https://docs.ollama.com/api/openai-compatibility); имена/defaults зависят от metadata модели. Boolean-only thinking модели отображают `none` в `false`. Для GPT-OSS отсутствие thinking output не доказывает отсутствие reasoning compute или billing. Засеяны только embedding-модели. |
| Qwen | OpenAI-compatible `forward`; контрол не смоделирован | [`enable_thinking`](https://www.alibabacloud.com/help/en/model-studio/deep-thinking) — не-OpenAI параметр через `extra_body`; `reasoning_effort` применим только к Omni. |
| LiteLLM | OpenAI-compatible `forward`; контрол не смоделирован | [`reasoning_effort` / native `thinking`](https://docs.litellm.ai/docs/reasoning_content), SDK/Proxy v1.63.0+. Поддержка зависит от routed model; продолжение Anthropic tools требует `thinking_blocks`. |
| TypeSafe | Настроенный decision endpoint; генеративное рассуждение unsupported | [Настроенный endpoint](https://api.typesafe.ai); публичной первичной документации генеративного контрола не получено. Decision-only контракт приложения не доказывает возможности всех продуктов вендора. |

Аудит различает две политики провайдера, смоделированные в коде, семь
документированных, но не смоделированных семейств контролов и три неподтверждённых
контрола вендоров. Поддержка native transport, endpoint policy и совместимость
конкретной модели — разные утверждения; они не подменяют типизированную проверку
привязки.

Уточнения моделей и транспорта из принятого аудита:

- Opus 5.5 по умолчанию использует `medium` и отвергает
  `thinking.type=disabled` на каждом уровне. Opus 5 по умолчанию использует `high`
  и отвергает `disabled` только при `xhigh`/`max`.
- Snake-case `thinking_level` Gemini относится к Interactions API, а не к
  настроенному `generateContent`. Поддержку уровней Gemini 3+ нельзя переносить
  на более ранние модели.
- Astra/Sol Chat Completions не поддерживает function calling. Одной
  совместимости reasoning effort недостаточно для допуска модели к Agent.
- Незаданный контрол не равен литералу `none`: построение запроса должно
  применять политику binding/provider, а явное `none` — соблюдать ограничения
  модели. Аудит не объявляет готовыми selector или типизированную wire validation:
  это отдельная продуктовая работа.

Для этой документации не выполнялись provider calls и изменения активных привязок.
