# Запуск IronRAG с Ollama

Локальная Ollama-интеграция: какие модели куда подходят, как
PostgreSQL vector storage увязан с размерностью эмбеддинга,
оперативные нюансы и пример набора профилей под 12 ГБ потребительский GPU.

Подробная версия и контракт лежат в [docs/en/OLLAMA.md](../en/OLLAMA.md).

## Почему Ollama

Ollama отдаёт OpenAI-совместимый API на
`http://<host>:11434/v1`, поэтому IronRAG общается с ней тем же
`openai_compatible`-адаптером, что и с облачной OpenAI / DeepSeek /
OpenRouter. Никакого ollama-специфичного кода в IronRAG нет — всё
ниже это конфигурация.

Бери Ollama туда, где хочешь оставить инференс локально: стадии
ingest (`embed_chunk`, `extract_graph` и опциональная `extract_text`) —
очевидные кандидаты, потому что они выполняются один раз на ревизию
и латентность прячется за очередью воркера. `query_answer` лучше
оставить на топ-облаке: эта стадия запускается на каждом turn'е, и
качество ответа — то, что видит пользователь.

Контракт конфигурации остаётся одним и тем же: ровно пять обязательных
профилей — `extract_graph`, `embed_chunk`, `query_compile`, `query_answer` и
`agent`. Мультимодальный `extract_text` опционален независимо от того, какие
профили работают локально.

## Пример набора профилей (12 ГБ VRAM, одна карта)

WARM-бенчмарк на RTX 5070 (12 ГБ) на представительном extract_graph
промпте над Rust-чанком:

| Назначение     | Модель                 | Латентность | Качество                          | VRAM   |
|----------------|------------------------|-------------|-----------------------------------|--------|
| `embed_chunk`  | `qwen3-embedding:0.6b` | 59 мс       | 1024-d, code-aware                | 1 ГБ   |
| `extract_graph`| `llama3.1:8b`          | 3.1 с       | JSON_OK, 11 entities / 8 relations | 5.5 ГБ |
| `extract_text` | `qwen3-vl:4b`          | —           | мультимодальный chat (PDF OCR)    | 3.3 ГБ |
| `query_answer` | cloud-модель           | —           | без изменений                     | 0      |

Чем НЕ пользоваться:

- **`qwen3:4b` / `qwen3:8b`** — пишут ~800 токенов `<thinking>…</thinking>`
  до полезного вывода. Ollama пока не уважает `/no_think` через
  OpenAI-совместимый API. Итог — пустой JSON. Пропускать до
  поддержки thinking-бюджета.
- **`phi4-mini`** — быстрая (~2 с), JSON валидный, но 5 сущностей
  против 11 у llama3.1 на том же промпте. Подходит если нужна голая
  скорость.
- **`gemma3:4b`** — высокий cold start (~66 с при первом вызове, ~3 с
  warm), оборачивает JSON в markdown-fence. Рабочий вариант, но не
  лучше llama3.1.

## Размерность векторов: per library

PostgreSQL хранит chunk/entity embeddings в per-`(library, dim)`
pgvector relations, учтённых в vector manifest. Один deployment может
одновременно держать библиотеки с разными active embedding dimensions.

Что это значит на практике:

- Active `embed_chunk` профиль библиотеки определяет embedding dimension и
  coordinate space для stored и query vectors.
- Переключение одной библиотеки с 3072-dimensional embedding model на
  `qwen3-embedding:0.6b` (1024 dim) не заставляет весь deployment
  использовать 1024 dim.
- Existing vector material затронутой библиотеки всё равно нужно
  перестроить до того, как retrieval начнёт использовать новую
  embedding model.

```bash
docker exec ironrag-backend-1 ironrag-maintenance rebuild vector-plane --source-library <library-uuid>
```

## Контекст и таймаут

Дефолтный `num_ctx` чат-моделей Ollama — **4096 токенов**. Длинные
README или дизайн-доки режутся в середине чанка, и extract_graph
теряет сущности с хвоста. Переопределяй `num_ctx` в нужном binding:

```json
{
  "temperature": 0.0,
  "extraParametersJson": { "num_ctx": 8192 }
}
```

8192 хватает на дефолтный chunk-size и любой разумный system prompt.
Эмбеддинг-модели контекстом не пользуются — поставь `num_ctx: 2048`
им просто для чистоты.

Второй рычаг — idle-таймаут IronRAG. Дефолт 300 с расчитан на cloud.
На llama3.1:8b с ~3 с/chunk большие .md файлы (100+ чанков) не
успевают:

```bash
# .env — читается docker compose автоматически
IRONRAG_RUNTIME_GRAPH_EXTRACT_IDLE_TIMEOUT_SECONDS=1800
```

Backend **и** worker пересоздавать вместе, затем рестартовать или
пересоздавать frontend. Frontend nginx резолвит upstream `backend` на
старте, поэтому после пересоздания backend `/v1/*` может смотреть в
stale Docker IP до рестарта nginx.

## Типовые сбои

| Симптом | Причина | Фикс |
|---|---|---|
| `ProviderUnavailable: failed to resolve chunk embedding dimensions for <uuid>` | Vector от embedding-модели не совпадает с active library profile или vector manifest lane | Проверь профиль `embed_chunk` библиотеки и запусти `ironrag-maintenance rebuild vector-plane --source-library` для затронутой библиотеки. |
| `graph extraction idle timeout: no chunk completed within 300s` | Локальная LLM медленнее таймаута | Поднять `IRONRAG_RUNTIME_GRAPH_EXTRACT_IDLE_TIMEOUT_SECONDS`. Перезапустить worker. |
| qwen3:* в extract пишет пустой JSON | 800 токенов `<thinking>` до содержимого; `/no_think` не уважается через OpenAI API | Поставить non-thinking модель: llama3.1:8b, phi4-mini, gemma3:4b. |
| Первый вызов после 5 мин простоя в 10× медленнее | `OLLAMA_KEEP_ALIVE=5m` выгрузил модель из VRAM | Поднять `OLLAMA_KEEP_ALIVE`. |
| Health на :19000 OK, но `/v1/*` отдают 404 | nginx frontend смотрит на stale backend IP после `--force-recreate backend` | Пересоздать ещё и frontend. |

## Тюнинг Ollama runtime

`/data/docker/ollama/docker-compose.yml`:

```yaml
environment:
  OLLAMA_KEEP_ALIVE: 30m     # без свопа в середине батча
  OLLAMA_NUM_PARALLEL: 2
  OLLAMA_MAX_LOADED_MODELS: 2
```

На 12 ГБ держать одновременно embedding + LLM можно впритык
(1 + 5.5 = 6.5 ГБ + контекст-буферы). Параллельный вызов мультимодального извлечения
вытолкнёт одну из них.

## См. также

- [docs/en/OLLAMA.md](../en/OLLAMA.md) — полная версия с
  бенчмарк-рецептом, ссылками на исходники и расширенной
  таблицей сбоев.
- `apps/api/src/bin/ironrag_maintenance.rs` — исходник CLI пересборки.
- `apps/api/src/services/query/search.rs:470` —
  `rebuild_vector_plane_for_library`.
- `apps/api/src/services/query/vector_dimensions.rs` — fail-loud
  проверка совпадения размерности.
