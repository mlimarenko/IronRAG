export default {
  page: {
    eyebrow: 'Интеграции',
    title: 'API и интеграции',
    description:
      'Живая страница интеграций: инвентарь токенов, backend endpoints, готовые примеры и project-scoped guidance на текущих поверхностях RustRAG.',
    loadingTitle: 'Загрузка API и integration context',
    actions: {
      refresh: 'Обновить API surface',
    },
    states: {
      loading: 'Загрузка',
      blocked: 'Заблокировано',
      foundation: 'Нужна настройка',
      ready: 'Готово',
    },
    errors: {
      title: 'Поверхность API/integrations недоступна',
      unknown: 'Неизвестная ошибка API/integrations',
    },
    empty: {
      noWorkspaceTitle: 'Не выбрано workspace',
      noWorkspaceMessage:
        'Выберите workspace, прежде чем рендерить инвентарь токенов и integration guidance.',
      noWorkspaceHint:
        'Страница workspace-first, потому что токены, governance и доступ к проектам уже завязаны на workspace ID.',
    },
  },
  workspace: {
    eyebrow: 'Контекст workspace',
    description: 'Живая integration guidance для {slug}.',
  },
  inventory: {
    eyebrow: 'Инвентарь интеграций',
    title: 'Текущий backend-backed inventory',
    description:
      'Панель честно показывает только то, что уже существует: token summaries, workspace governance counts, проекты и addressable endpoints.',
    ready: 'Поверхность загружена',
    needsTokens: 'Нужны seed-данные',
    cards: {
      tokens: 'API токены',
      tokensHint: 'Токены workspace из /v1/auth/tokens.',
      projects: 'Проекты',
      projectsHint: 'Доступен project scope для document и query вызовов.',
      providerAccounts: 'Provider accounts',
      providerAccountsHint: 'Счётчик из workspace governance для readiness интеграций.',
      modelProfiles: 'Model profiles',
      modelProfilesHint: 'Количество профилей для query и ingest flows.',
    },
  },
  foundation: {
    workspaceContext: 'Workspace выбран для scoped examples',
    tokenInventory: 'Инвентарь токенов загружен из auth surface',
    projectScope: 'Есть хотя бы один проект для integration examples',
    providerReadiness: 'Настройка providers и model profiles уже есть',
    ready: 'Готово',
    todo: 'Нужна настройка',
  },
  tokens: {
    eyebrow: 'Инвентарь токенов',
    title: 'Инвентарь API токенов',
    description:
      'Read-only summaries из backend. Plaintext token после создания намеренно больше не показывается, поэтому тут фокус на label, наборе scopes и свежести использования.',
    available: 'Инвентарь виден',
    emptyBadge: 'Нет токенов',
    createdAt: 'Создан',
    lastUsedAt: 'Последнее использование',
    never: 'Никогда',
    emptyTitle: 'API токенов пока нет',
    emptyMessage:
      'Сначала выпустите workspace- или instance-token, иначе примеры интеграции вне UI не заработают.',
    emptyHint:
      'Backend уже умеет POST /v1/auth/tokens. Эта страница специально не притворяется полноценным secret minting UX.',
    scopeInventoryTitle: 'Инвентарь scopes',
    scopeInventoryDescription: 'Агрегированные scopes по всем видимым токенам.',
  },
  examples: {
    eyebrow: 'Панель примеров',
    title: 'Copy-paste примеры для backend',
    description:
      'Примеры привязаны к выбранному workspace и опциональному проекту, чтобы будущие SDK snippets опирались на ту же структуру.',
    liveSurface: 'Текущие backend routes',
    shared: {
      tokenNote: 'Подставьте реальный bearer token, например {token}.',
      workspaceScopeNote:
        'Workspace-scoped токены могут работать только внутри своего workspace; project ID тоже должен принадлежать ему.',
    },
    cards: {
      workspaceGovernance: {
        title: 'Прочитать workspace governance',
        description:
          'Получить counts по providers, profiles, tokens и usage для выбранного workspace.',
        note: 'Полезно для admin dashboards и install-time health checks.',
      },
      projectDocuments: {
        title: 'Список документов проекта',
        description: 'Проверить контент, уже привязанный к {project}.',
        note: 'Документы остаются project-scoped, даже если токен только workspace-scoped.',
      },
      runQuery: {
        title: 'Запустить grounded query',
        description: 'Дёрнуть retrieval+answer путь по выбранному проекту.',
        note: 'Выбор model profile в body пока остаётся опциональным, если хватает backend defaults.',
      },
    },
  },
  guidance: {
    eyebrow: 'Project-scoped guidance',
    title: 'Интеграционная guidance',
    description:
      'Этот блок задаёт структуру для per-project setup docs и не выдумывает backend capabilities, которых ещё нет.',
    projectScoped: 'Скоуп проекта',
    workspaceScoped: 'Скоуп workspace',
    allProjects: 'Вид по всему workspace',
    workspaceTitle: 'Интеграционная guidance для {workspace}',
    workspaceDescription:
      'Используйте этот режим, когда собираете admin или multi-project tooling и знаете только workspace на старте.',
    projectTitle: 'Интеграционная guidance для {project}',
    projectDescription:
      'Используйте {project}, когда нужны content, retrieval или document API в рамках одного проекта внутри {workspace}.',
    bullets: {
      scopeWorkspace:
        'Держите workspace slug ({workspace}) как человекочитаемый якорь в документации и operator tooling.',
      scopeProject:
        'Сохраняйте project slug ({project}) рядом с project ID, чтобы интеграции оставались дебажными.',
      tokenReuse:
        'Лучше переиспользовать внятно подписанный workspace token вроде «{token}», чем безымянные локальные секреты.',
      tokenMissing:
        'Видимого workspace token сейчас нет, значит внешним интеграциям всё ещё нужен отдельный шаг выпуска токена.',
      permissionsScoped:
        'Неглобальный token уже есть, и это правильная база для project-scoped automation без лишнего instance-admin размаха.',
      permissionsAdminFallback:
        'Если сейчас у вас только instance-admin токены, считайте их bootstrap credentials и планируйте даунскоуп позже.',
    },
  },
  endpoints: {
    eyebrow: 'Integration endpoints',
    title: 'Известные backend endpoints',
    description:
      'Здесь показываются base URL и самые полезные routes для выбранного workspace и project контекста.',
    configured: 'Endpoint настроен',
    unconfigured: 'Endpoint отсутствует',
  },
} as const
