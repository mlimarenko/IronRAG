# AI bindings (привязки моделей)

*AI binding* — это связка runtime-цели (скомпилировать запрос,
посчитать эмбеддинг, ответить на вопрос, …) с конкретным
provider + model + preset. Все LLM-стадии IronRAG резолвят свою привязку
через одну функцию: `AiCatalogService::resolve_active_runtime_binding`.

Документ описывает контракт, список purpose'ов, иерархию scope'ов и что
учитывать при выборе модели для каждого purpose'а.

## 1. Модель данных

Строка в `ai_binding_assignment` склеивает шесть кусков:

| Поле | Источник | Назначение |
|---|---|---|
| `binding_purpose` | enum `ai_binding_purpose` | один из 10 purpose'ов ниже |
| `scope_kind` | enum `ai_scope_kind` | `instance` / `workspace` / `library` |
| `workspace_id`, `library_id` | nullable | заполняется согласно `scope_kind` |
| `provider_credential_id` | `ai_provider_credential` | api key + base URL + provider catalog |
| `model_preset_id` | `ai_model_preset` | model + temperature + top_p + опц. system prompt |
| `binding_state` | enum `ai_binding_state` | `active`, `inactive`, … |

Runtime резолвит **эффективную** привязку, обходя scope-лестницу:

1. library-scoped active binding для `(library_id, purpose)`
2. workspace-scoped active binding для `(workspace_id, purpose)`
3. instance-scoped active binding только для `(purpose)`

Первое совпадение выигрывает. Если ничего не настроено — стадия падает
громко (никаких тихих fallback на default-провайдера).

## 2. Десять purpose'ов

| Purpose | Стадия-потребитель | Что модель должна уметь |
|---|---|---|
| `extract_text` | ingest: text/code/image OCR fallback | структурное извлечение plain-текста из «грязных» источников |
| `extract_graph` | ingest: graph builder | строгий JSON-tagging сущностей/связей по чанку |
| `embed_chunk` | ingest: vector indexer | эмбеддинги (не chat); размерность обязана совпадать с per-library шардом |
| `query_compile` | query: NL-вопрос → `QueryIR` | строгий JSON по фиксированной схеме; низкая температура |
| `query_retrieve` | query: опциональный rewriter / HyDE | короткая генерация текста; нужен только HyDE/CRAG |
| `query_answer` | query: grounded ответ по retrieved bundle | citation-aware длинная генерация; instruction following |
| `vision` | ingest: image-to-text на визуальных чанках | мультимодальная модель с image input |
| `utility` | misc background helpers | любая chat-capable модель |
| `rerank` | retrieval: опциональный cross-encoder | dedicated rerank-модель (Cohere, Jina, BGE), не chat |
| `agent` | UI in-product agent + MCP host `grounded_answer` | tool-calling chat-модель с хорошим instruction following |

## 3. Wire-level структура prompt'а (это важно для кэширования)

`build_structured_chat_request` собирает `ChatRequest`, который путь
`generate()` сериализует в такую форму (см.
`apps/api/src/integrations/llm/openai_compatible.rs`):

```jsonc
{
  "model": "<model_name>",
  "messages": [
    { "role": "system", "content": "<preset.system_prompt ИЛИ встроенный prompt purpose'а>" },
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
- `extra_parameters_json` на credential'е и preset'е сливается в тело
  как есть. **Никаких per-call динамических значений** (user id,
  request id, timestamp, …). Любая динамика убивает provider-side
  prompt cache и каждый вызов платит полную latency.

## 4. Выбор модели по purpose'у

Реальные tradeoff'ы latency/качество/цена — provider-specific. Со
стороны IronRAG важна *форма* работы:

### `query_compile`

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

### `query_answer`

- Длинная генерация с inline citations. Output доминирует в latency.
- Streaming помогает; UI assistant и MCP grounded-answer tool оба
  потребляют streamed-деltas.
- Более качественная модель обычно отбивает разницу — это видимый
  пользователю продукт.

### `embed_chunk`

- Не chat-модель. Размерность — часть per-library контракта,
  векторная коллекция шардирована по dim
  (`knowledge_chunk_vector_d<dim>`).
- Переход на модель с другой размерностью требует
  `ironrag-maintenance migrate vector-per-dim` и переиндексации.
- Latency важна на ingest-масштабе, не per query.

### `extract_graph`

- Строгий JSON output per chunk; запускается многократно на документ.
  Cost и throughput доминируют над single-call latency.
- Маленькая быстрая модель со structured output обычно ок.

### `rerank`

- Cross-encoder reranker, **не** chat-модель. Берите dedicated
  reranker (Cohere, Jina, BGE) — chat-модели здесь не работают.

### `vision` / `extract_text`

- Vision-возможности активного провайдера определяют, идут ли image
  chunks через Docling OCR или через LLM vision. Без активной
  `vision`-привязки image chunks деградируют до text-extraction.

### `agent`

- Tool-calling chat-модель: UI in-product agent и MCP host для
  `grounded_answer`. Должна аккуратно следовать tool-call схемам и
  выбирать правильный tool с правильными аргументами.

## 5. Инспекция активных привязок

Привязки живут в Postgres. Этот запрос склеивает шесть таблиц и
показывает каждую активную привязку с её provider'ом, моделью и preset'ом:

```sql
SELECT
  b.scope_kind,
  b.workspace_id,
  b.library_id,
  b.binding_purpose,
  p.provider_kind,
  m.model_name,
  octet_length(coalesce(mp.system_prompt,'')) AS sys_prompt_bytes,
  mp.temperature,
  mp.top_p,
  mp.max_output_tokens_override
FROM ai_binding_assignment b
JOIN ai_provider_credential pc ON pc.id = b.provider_credential_id
JOIN ai_provider_catalog p     ON p.id  = pc.provider_catalog_id
JOIN ai_model_preset mp        ON mp.id = b.model_preset_id
JOIN ai_model_catalog m        ON m.id  = mp.model_catalog_id
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
FROM ai_binding_assignment b, library
JOIN ai_provider_credential pc ON pc.id = b.provider_credential_id
JOIN ai_provider_catalog p     ON p.id  = pc.provider_catalog_id
JOIN ai_model_preset mp        ON mp.id = b.model_preset_id
JOIN ai_model_catalog m        ON m.id  = mp.model_catalog_id
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
- **Embedding dimension drift.** Смена `embed_chunk`-модели между
  моделями разной размерности (например `1024` → `3072`) требует
  vector-migrate и переиндексации. Per-dim Arango-шард делает
  роутинг; старый шард остаётся пока не мигрирован.
- **Динамика в `extra_parameters_json`.** Всё, что меняется
  per-request (user id, timestamp, request id), попадает в тело и
  убивает provider-side prompt cache.
- **Правки system prompt.** Изменение встроенного
  `QUERY_COMPILER_SYSTEM_PROMPT` или `EXTRACT_GRAPH_SYSTEM_PROMPT`
  инвалидирует prompt cache у каждой привязки, которая использует
  встроенный prompt. Относитесь к этим константам как к части схемы и
  версионируйте через `QUERY_IR_SCHEMA_VERSION` в IR cache key.
- **Микс scope'ов.** Library-override полностью прячет workspace и
  instance fallback'и. Если поставили library-scoped
  `query_compile`-привязку, workspace и instance для этой библиотеки
  не используются. Используйте scope только когда реально нужен
  per-library / per-workspace override.
- **Операторский `max_output_tokens_override` сохраняется после
  рестартов.** Startup seed заполняет `max_output_tokens_override`
  только если у preset-строки это поле ещё не задано. Поднятый
  оператором output budget (например для preset'а graph-extraction)
  сохраняется между рестартами backend'а и не откатывается молча к
  значению из catalog-дефолта.

## 7. Смена активной привязки

1. Добавьте модель в `ai_model_catalog`, если её там нет.
2. Создайте preset в `ai_model_preset` (temperature, top_p, опц.
   override system prompt, `max_output_tokens_override`).
3. Создайте credential в `ai_provider_credential` для выбранного
   `ai_provider_catalog`.
4. Вставьте `ai_binding_assignment` с нужным `scope_kind`,
   `binding_purpose`, `provider_credential_id`, `model_preset_id`,
   `binding_state = 'active'`. Уникальный индекс на
   `(scope, purpose)` гарантирует single-active на scope.
5. Прогоните регрессионный бенч `scripts/bench/agent_turn_p95.py`
   на затронутых библиотеках до коммита новой привязки.

См. `docs/ru/PIPELINE.md` про чейн bindings в ingest-пайплайне и
`docs/ru/BENCHMARKS.md` про bench harness.
