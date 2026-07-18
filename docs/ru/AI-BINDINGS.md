# AI bindings (привязки моделей)

*AI binding profile* — это связка одной понятной оператору
задачи (понять запрос, посчитать векторы, сформировать ответ, …) с
`ai_account` и catalog model. Prompt и sampling settings хранятся inline в этом
binding. Все LLM-стадии IronRAG резолвят физический purpose через одну функцию:
`AiCatalogService::resolve_active_runtime_binding`.

В UI осталось ровно пять обязательных профилей и опциональный
`extract_text`. Runtime-стадии эмбеддинга запроса, semantic rerank и визуального
анализа напрямую используют канонический профиль и не являются отдельными
настраиваемыми привязками.

## 1. Модель данных

Строка в `ai_binding` хранит следующие части:

| Поле | Источник | Назначение |
|---|---|---|
| `binding_purpose` | enum `ai_binding_purpose` | один из внутренних runtime-purpose'ов ниже |
| `scope_kind` | enum `ai_scope_kind` | `instance` / `workspace` / `library` |
| `workspace_id`, `library_id` | nullable | заполняется согласно `scope_kind` |
| `account_id` | `ai_account` | API credential + base URL + provider catalog |
| `model_catalog_id` | `ai_model_catalog` | типизированная модель и её capabilities |
| prompt/sampling-поля | внутри `ai_binding` | system prompt, temperature, top_p, output override, extra parameters |
| `binding_state` | enum `ai_binding_state` | только `active`, `invalid` или `disabled` |

Runtime резолвит **эффективную** привязку, обходя scope-лестницу:

1. library-scoped active binding для `(library_id, purpose)`
2. workspace-scoped active binding для `(workspace_id, purpose)`
3. instance-scoped active binding только для `(purpose)`

Первое совпадение выигрывает. Если канонический профиль не настроен, стадия
падает громко. Случайная модель или provider не подставляются.

## 2. Профили в UI и внутренние purpose'ы

Обычная настройка теперь короткая:

| Профиль | Обязательный | Канонический purpose | Контракт |
|---|---:|---|---|
| Извлечение графа | да | `extract_graph` | строгий JSON типизированных сущностей и связей |
| Векторные представления | да | `embed_chunk` | одна embedding-модель для сегментов индекса и векторов запроса |
| Понимание запроса | да | `query_compile` | типизированный `QueryIR` и semantic rerank отдельными runtime-вызовами |
| Генерация ответов | да | `query_answer` | grounded-ответ с цитатами |
| AI-ассистент | да | `agent` | tool-calling host для UI и MCP |
| Понимание документов | нет | `extract_text` | сложный текст/OCR/визуальный анализ после детерминированных парсеров |

Эти шесть канонических purpose'ов — весь binding enum. Runtime-события и
billing сохраняют собственную типизированную идентичность стадии, не создавая
лишних профилей моделей:

| Внутренний purpose | Стадия-потребитель | Контракт резолвинга |
|---|---|---|
| `extract_text` | ingest: сложный текст/OCR и визуальный анализ | канонический мультимодальный профиль «Понимание документов» |
| `extract_graph` | ingest: graph builder | строгий JSON-tagging сущностей/связей по чанку |
| `embed_chunk` | ingest и query: индексирование сегментов и эмбеддинг вопроса | один канонический профиль и vector space |
| `query_compile` | query: построение `QueryIR` и опциональный rerank evidence | один профиль «Понимание запроса»; отдельные вызовы, схемы, бюджеты и accounting |
| `query_answer` | query: grounded ответ по retrieved bundle | citation-aware длинная генерация; instruction following |
| `agent` | UI in-product agent + MCP host `grounded_answer` | tool-calling chat-модель с хорошим instruction following |

Допуск модели определяется типизированными capability/modality в каталоге. Он никогда
не угадывается по provider/model name, суффиксу или ручному словарю.

Для `agent` действует более строгая комбинация: у модели должна быть явно
задана catalog-role `agent` и capability kind `chat`, а provider profile обязан
объявлять и `chat`, и `tools` как `supported`. Значение `unknown` поддержкой не
считается. Upgrade-миграция 0012 материализует Agent eligibility и
каноническую Agent bootstrap entry из `query_answer` только при таком
типизированном контракте provider. Уже существующая отдельная конфигурация Agent
сохраняется, но не превращается в runtime-fallback.

Если provider не возвращает typed metadata в model-list, генератор каталога
принимает operator manifest через `IRONRAG_AI_MODEL_CAPABILITIES_JSON_B64`. После
base64-декодирования это object с provider kind на первом уровне и точной
model identity на втором. Каждая запись содержит только `capabilityKind`
(`chat` или `embedding`) и `modalityKind` (`text` или `multimodal`). Манифесты
с дублями или превышением лимита отклоняются; записи с некорректной сигнатурой и
модели без typed declaration пропускаются.

## 3. Wire-level структура prompt'а (это важно для кэширования)

`build_structured_chat_request` собирает `ChatRequest`, который путь
`generate()` сериализует в такую форму (см.
`apps/api/src/integrations/llm/openai_compatible.rs`):

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
ценового tier по размеру контекста. В текущей price-схеме нет отдельного
cache-write unit, поэтому cache-creation Anthropic явно аппроксимируется по
обычной input-ставке, а cache-read остаётся отдельным cached-input charge.

## 4. Выбор модели по профилю

Реальные tradeoff'ы latency/качество/цена — provider-specific. Со
стороны IronRAG важна *форма* работы:

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

Provider-backed rerank всегда использует эту же модель. Компиляция и rerank
остаются отдельными runtime-вызовами со своими схемами, deadline, лимитами
конкурентности, трассировкой и accounting.

### `query_answer`

- Длинная генерация с inline citations. Output доминирует в latency.
- Streaming помогает; UI assistant и MCP grounded-answer tool оба
  потребляют streamed-деltas.
- Более качественная модель обычно отбивает разницу — это видимый
  пользователю продукт.

#### Rollout semantic rerank

Provider-backed semantic rerank включается явно и по умолчанию имеет режим
`IRONRAG_QUERY_SEMANTIC_RERANK_MODE=off`. Безопасный rollout:
`off` -> `shadow` -> `active`. Настройка `IRONRAG_QUERY_RERANK_ENABLED` —
master gate: startup отклоняет `shadow` или `active`, если она равна `false`.

- `off` продолжает использовать детерминированную lexical-эвристику с resolved
  standalone retrieval question и не делает вызов provider.
- `shadow` отправляет resolved standalone retrieval question и ограниченные
  фрагменты кандидатов в активный профиль «Понимание запроса» через одну ограниченную на
  процесс low-priority задачу только при настоящем result-cache miss. Порядок
  ответа не меняется, а query path не ждёт ответа provider. Обычные cache hit
  остаются быстрыми и не создают повторный shadow-вызов или billing-запись;
  историческое переиспользование явно фиксируют query-execution replay row и
  cache-hit log.
- `active` ждёт только валидный provider ranking. Настроенный timeout с жёстким
  пределом 3000 мс — это decision budget, который начинается до lookup binding.
  Lookup binding и durable reservation не отменяются посреди операции с БД, но
  расходуют этот budget; provider получает только оставшееся время. Если budget
  закончился после reservation, известная запись terminalize'ится, а provider
  не вызывается. При отсутствии binding, timeout, ошибке provider, некорректном
  JSON или ошибке accounting используется та же детерминированная lexical
  fallback-эвристика. Обязательный completion/accounting после ответа provider
  может добавить DB overhead уже за пределами decision deadline.

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

### Векторные представления (один профиль для индекса и запросов)

- Это одна операторская привязка, а не две независимо выбираемые модели.
  `embed_chunk` индексирует чанки, а query-путь резолвит этот же профиль для
  вопроса. Второго purpose для query embedding нет.
- Каждый сохранённый вектор и query lookup используют secret-free ключ
  execution-профиля `embedding-profile:v1:<sha256>`. Он учитывает resolved
  provider/model path и канонические request parameters, но не scope, binding
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
  число значений не изменилось. Старые UUID-keyed векторы после обновления тоже
  требуют однократного rebuild.
- Если у библиотеки есть активный source material, но нет векторов точного
  активного профиля, retrieval возвращает явное требование rebuild, а не тихий
  lexical-only или пустой vector result.
- Действительно пустая библиотека — корректное типизированное состояние.
  Query preflight не вызывает embedding provider и ANN lanes и не требует
  невозможного rebuild.
- Rebuild потоково читает канонические chunks через keyset cursor, пишет
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
  OCR. Каноническая мультимодальная модель понимания документов берёт сложное
  извлечение и визуальный контент, где нужно model reasoning.
- Визуальный анализ использует эту же модель. Привязка принимается, только если
  каталог объявляет chat-capable мультимодальную модель, а provider поддерживает
  визуальный input. Без подходящего профиля остаётся детерминированное
  извлечение, а model-only анализ недоступен.

### `agent`

- Tool-calling chat-модель: UI in-product agent и MCP host для
  `grounded_answer`. Должна аккуратно следовать tool-call схемам и
  выбирать правильный tool с правильными аргументами.

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

1. Убедитесь, что model discovery или operator capability manifest создали
   строку `ai_model_catalog` с нужными typed capability/modality.
2. Создайте или выберите `ai_account` для provider.
3. Через UI администрирования или AI configuration API сохраните логический
   профиль на нужном scope. Модель, prompt, sampling, output override и extra parameters
   хранятся прямо в `ai_binding`; не пишите в таблицу вручную.
4. Прогоните регрессионный бенч `scripts/bench/agent_turn_p95.py`
   на затронутых библиотеках до коммита новой привязки.

См. `docs/ru/PIPELINE.md` про чейн bindings в ingest-пайплайне и
`docs/ru/BENCHMARKS.md` про bench harness.
