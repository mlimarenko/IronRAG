# IronRAG frontend

Документ фиксирует текущую структуру web frontend и QA-контракт для `apps/web`.

## Структура каталогов

```text
apps/web/src/
├── app/             App shell, root-роутинг и shell-level компоненты
├── features/        Page-owned feature packages (по одному каталогу на surface)
│   └── {feature}/   page shell, components/, model/ (adapters), hooks/, workers/
└── shared/          Cross-feature строительные блоки
    ├── api/         Тонкие HTTP-клиенты для `/v1/*`
    ├── components/  Переиспользуемые view-компоненты (включая ui/ presentation-примитивы)
    ├── contexts/    Глобальное app state, включая active workspace и library
    ├── forms/       Общие form helpers
    ├── hooks/       Cross-page React hooks
    ├── i18n/        Locale-ресурсы
    ├── lib/         Утилиты без React
    ├── test/        Сквозные UI-аудиты
    └── types/       Общие frontend domain types
```

## Frontend-контракты

- `shared/api/*` ходит в backend и возвращает wire payload либо уже нормализованный DTO.
- `features/*/model/*Adapter.*` — единственная зона, где Raw API envelope превращается в domain model.
- `features/*` оркестрируют загрузку данных, routing state и page-owned derived state.
- `shared/components/*` рендерят и не владеют transport-логикой.
- Page-specific helpers живут рядом со страницей в `features/{feature}/`.
- `shared/components/ui/*` остаются presentation-only.

## Владение страницами

### Dashboard

- Использует `/v1/ops/libraries/{libraryId}/dashboard` и `/v1/ops/libraries/{libraryId}`.
- Строит summary cards, health rows, recent documents и ingest status из одного dashboard payload.
- Refresh не должен перерисовывать всю страницу.

### Documents

- Владеет keyset-пагинацией, upload, batch-action, inspector, web-run list и входом в editor.
- Использует основной list endpoint `/v1/content/documents`.
- Строки показывают active ingest progress из document list payload; отдельный polling lane для per-row процентов не нужен.
- Inspector detail, prepared segments, technical facts, revisions и source download грузятся отдельными endpoint'ами.
- Inspector pipeline state строится из stage read-model data: stage status, progress, duration, model, cost, provider-call count и extraction/chunk details.
- Прогресс batch rerun поллится через `/v1/ops/operations/{operationId}`.

### Assistant

- Владеет списком сессий, активной сессией, историей сообщений, pending-turn state и debug context.
- Использует `/v1/query/sessions/*` для session CRUD и turn execution.
- Turn execution использует один `POST /v1/query/sessions/{sessionId}/turns` request. UI запрашивает `text/event-stream`, чтобы activity, failure и completion events обновляли pending answer bubble, а completed answer оставался persisted session/execution record.
- Если browser или proxy роняет stream после старта backend work, client перечитывает durable session result, созданный после request boundary, вместо повторной отправки turn. Backend `failed` events остаются terminal errors.
- LLM context debug загружает persisted execution snapshots, а не process-local cache, поэтому reload и cached answer replay остаются inspectable при наличии snapshot.

### Graph

- Загружает topology через `/v1/knowledge/libraries/{libraryId}/graph`.
- Загружает summary через `/v1/knowledge/libraries/{libraryId}/summary`.
- Загружает entity detail по выбору через `/v1/knowledge/libraries/{libraryId}/entities/{entityId}`.
- Adjacency lookup централизован, чтобы inspector считал соседей только для выбранного узла.
- Layout computation выполняется в Web Worker начиная с 3000 узлов. Первый canvas paint ~1.6 с на графе из 25k узлов.
- Подписи узлов отключаются при >15k узлов. Анимация layout пропускается при >5k узлов.
- Hidden-edge precompute и O(degree) selection сохраняют отзывчивость на больших графах.

### Admin

- Использует `/v1/admin/surface` как shell bootstrap.
- Access, AI, pricing, audit, MCP prompt, snapshot и catalog operations владеют своими fetch path.
- Tabs монтируются лениво; неактивные вкладки не должны бесконечно рефетчить.

### Swagger

- Route `/swagger` встраивает `/swagger.html` через iframe.
- Vendor CSS Swagger UI изолирован от Tailwind app shell; страница грузит generated OpenAPI JSON через frontend origin.

## Frontend quality gates

### Static и unit tests

```bash
cd apps/web
npm run lint
npm test
```

### Visual QA

```bash
cd apps/web
QA_LOGIN=admin QA_PASSWORD='<password>' \
PLAYWRIGHT_BROWSERS_PATH=$HOME/.cache/ms-playwright \
npx playwright test --config=playwright.qa.config.ts
```

Playwright-сьют снимает живой UI на desktop и constrained mobile viewport и пишет скриншоты в `apps/web/visual-qa/screenshots/`.

### Ручные проверки

Нужны как минимум эти классы viewport:

| Viewport | Пример размера | Что проверить |
|---|---|---|
| Mobile | `375x812` | stacking layout, horizontal overflow, drawer sizing |
| Tablet | `768x1024` | collapse sidebar, wrap табов и панелей |
| Desktop | `1440x900` | основной операторский workflow |
| Wide | `1920x1080` | multi-column surfaces, ширина graph inspector |

Проверяй:

- Dashboard refresh не перестраивает всю страницу.
- Documents table остается usable на узких ширинах, web-run rows раскрываются inline.
- Assistant streaming обновляет только активный answer bubble, не ломает scroll behavior и не дублирует turn при transport recovery.
- Graph selection меняет inspector state без повторной загрузки topology stream.
- Admin tabs грузят только свои данные и остаются usable на узких ширинах.

## Порог качества

Frontend не считается готовым просто потому, что TypeScript собрался. Он готов, когда:

- layout держится на desktop и constrained widths,
- долгие поверхности стабильно рендерятся во время polling и streaming,
- empty/loading/error states читаемы,
- никакая страница не парсит Raw wire shape прямо в render tree.
