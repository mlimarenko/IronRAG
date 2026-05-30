# Запуск IronRAG с Ollama

Локальная Ollama-интеграция: какие модели куда подходят, как индекс
векторов в Arango увязан с размерностью эмбеддинга, оперативные
нюансы и готовый пресет под 12 ГБ потребительский GPU.

Подробная версия и контракт лежат в [docs/en/OLLAMA.md](../en/OLLAMA.md).

## Почему Ollama

Ollama отдаёт OpenAI-совместимый API на
`http://<host>:11434/v1`, поэтому IronRAG общается с ней тем же
`openai_compatible`-адаптером, что и с облачной OpenAI / DeepSeek /
OpenRouter. Никакого ollama-специфичного кода в IronRAG нет — всё
ниже это конфигурация.

Бери Ollama туда, где хочешь оставить инференс локально: стадии
ingest (`embed_chunk`, `extract_graph`, `vision`, `extract_text`) —
очевидные кандидаты, потому что они выполняются один раз на ревизию
и латентность прячется за очередью воркера. `query_answer` лучше
оставить на топ-облаке: эта стадия запускается на каждом turn'е, и
качество ответа — то, что видит пользователь.

## Рекомендуемый пресет (12 ГБ VRAM, одна карта)

WARM-бенчмарк на RTX 5070 (12 ГБ) на представительном extract_graph
промпте над Rust-чанком:

| Назначение     | Модель                 | Латентность | Качество                          | VRAM   |
|----------------|------------------------|-------------|-----------------------------------|--------|
| `embed_chunk`  | `qwen3-embedding:0.6b` | 59 мс       | 1024-d, code-aware                | 1 ГБ   |
| `extract_graph`| `llama3.1:8b`          | 3.1 с       | JSON_OK, 11 entities / 8 relations | 5.5 ГБ |
| `vision`       | `qwen3-vl:4b`          | —           | мультимодальный chat (PDF OCR)    | 3.3 ГБ |
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

## Размерность векторов: instance-wide, а не per-library

**Это самая частая ошибка операторов на первом запуске.**

Vector-индексы Arango (`knowledge_chunk_vector_index`,
`knowledge_entity_vector_index`) создаются с фиксированной
размерностью. Индекс **общий на весь инстанс**: per-library /
per-workspace индексов нет. Все библиотеки делят одну размерность.

Что это значит на практике:

- Размерность индекса определяет **первая** embedding-модель,
  зарегистрированная на деплое. Bootstrap по умолчанию ставит
  `text-embedding-3-large` (3072 dim).
- Смена embedding-модели на другую размерность (например, переход на
  `qwen3-embedding:0.6b` = 1024 dim) требует пересборки индекса.
- Пересборка тоже **instance-wide**: `ironrag-maintenance rebuild vector-plane --source-library
  <library-uuid>` читает binding целевой библиотеки, берёт оттуда
  новую размерность и переэмбеддит все библиотеки с живым vector
  material под одну новую размерность.

### Падение когда binding'и расходятся

```
cannot rebuild Arango vector plane to 1024 dimensions:
library <uuid> active vector binding produces 3072 dimensions
```

Это **honest fail** — смешивать размерности в одном деплое нельзя.

### Как чинить

Оба поддерживаются:

1. **Одна embedding-модель на весь деплой.** Поставить
   instance-level `embed_chunk` / `query_retrieve` binding на нужную
   модель, потом запустить `ironrag-maintenance rebuild vector-plane --source-library` против любой
   библиотеки с material. Workspaces наследуют instance-binding, если
   не переопределяют на своём уровне.

2. **Очистить расходящиеся библиотеки.** Hard-delete документов из
   библиотеки с другой размерностью (её векторы в
   `knowledge_chunk_vector` уходят). Precondition пересборки
   пропускает библиотеки без material, и mismatch уходит.

Сама пересборка атомарно:
1. Дропает оба vector-индекса.
2. Если размерность поменялась — truncate'ит обе vector-коллекции.
3. Переэмбеддит каждый чанк/entity каждой библиотеки с material,
   используя её активный binding.
4. Воссоздаёт индексы с новой размерностью.

Онлайн-режима нет: пересборка блокирует векторные записи. У нас
локальный IDE workspace с ~800 чанками собирается за 30 секунд; на
100k чанках — минуты.

```bash
docker exec ironrag-backend-1 ironrag-maintenance rebuild vector-plane --source-library <library-uuid>
```

## Контекст и таймаут

Дефолтный `num_ctx` чат-моделей Ollama — **4096 токенов**. Длинные
README или дизайн-доки режутся в середине чанка, и extract_graph
теряет сущности с хвоста. Переопределяй `num_ctx` в пресете:

```json
{
  "presetName": "IDE/extract/llama3.1-8b",
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

```yaml
# docker-compose-local.yml — ironrag-app-env
IRONRAG_RUNTIME_GRAPH_EXTRACT_IDLE_TIMEOUT_SECONDS: 1800
```

Backend **и** worker пересоздавать вместе; если трогаешь frontend —
по CLAUDE.md его тоже надо recreate (nginx upstream кэширует stale
backend IP).

## Типовые сбои

| Симптом | Причина | Фикс |
|---|---|---|
| `ProviderUnavailable: failed to resolve chunk embedding dimensions for <uuid>` | Vector от embedding-модели не совпадает с размерностью индекса | `ironrag-maintenance rebuild vector-plane --source-library`. Если падает с dimension-mismatch — сначала вычисти расходящуюся библиотеку. |
| `graph extraction idle timeout: no chunk completed within 300s` | Локальная LLM медленнее таймаута | Поднять `IRONRAG_RUNTIME_GRAPH_EXTRACT_IDLE_TIMEOUT_SECONDS`. Перезапустить worker. |
| qwen3:* в extract пишет пустой JSON | 800 токенов `<thinking>` до содержимого; `/no_think` не уважается через OpenAI API | Поставить non-thinking модель: llama3.1:8b, phi4-mini, gemma3:4b. |
| Первый вызов после 5 мин простоя в 10× медленнее | `OLLAMA_KEEP_ALIVE=5m` выгрузил модель из VRAM | Поднять `OLLAMA_KEEP_ALIVE`. |
| Health на :19000 OK, но `/v1/*` отдают 404 | nginx frontend смотрит на stale backend IP после `--force-recreate backend` | Пересоздать ещё и frontend (см. CLAUDE.md). |

## Тюнинг Ollama runtime

`/data/docker/ollama/docker-compose.yml`:

```yaml
environment:
  OLLAMA_KEEP_ALIVE: 30m     # без свопа в середине батча
  OLLAMA_NUM_PARALLEL: 2
  OLLAMA_MAX_LOADED_MODELS: 2
```

На 12 ГБ держать одновременно embedding + LLM можно впритык
(1 + 5.5 = 6.5 ГБ + контекст-буферы). Параллельный vision-вызов
вытолкнёт одну из них.

## См. также

- [docs/en/OLLAMA.md](../en/OLLAMA.md) — полная версия с
  бенчмарк-рецептом, ссылками на исходники и расширенной
  таблицей сбоев.
- `apps/api/src/bin/vector_rebuild.rs` — исходник CLI пересборки.
- `apps/api/src/services/query/search.rs:450` —
  `rebuild_vector_plane_from_library_binding`.
- `apps/api/src/services/query/vector_dimensions.rs` — fail-loud
  проверка совпадения размерности.
