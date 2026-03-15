export default {
  page: {
    eyebrow: 'API launchpad',
    title: 'API Start',
    description:
      'Понятная точка входа в текущий API surface RustRAG: публичные routes, настройка bearer token, scoped examples и groundwork для будущей документации.',
    technicalNote:
      'Это вторичная developer-поверхность для интеграций и автоматизации. Основной продуктовый путь по-прежнему живёт в Подготовке, Файлах и Поиске.',
    loadingTitle: 'Загрузка API launchpad',
    actions: {
      setup: 'Открыть setup',
      refresh: 'Обновить API context',
    },
    states: {
      loading: 'Загрузка',
      blocked: 'Заблокировано',
      foundation: 'Нужна настройка',
      ready: 'Готово',
    },
    errors: {
      title: 'API launchpad недоступен',
      unknown: 'Неизвестная ошибка API launchpad',
    },
    empty: {
      noWorkspaceTitle: 'Не выбран workspace',
      noWorkspaceMessage:
        'Создайте или выберите workspace, прежде чем рендерить scoped API guidance.',
      noWorkspaceHint:
        'API launchpad сделан workspace-first, потому что проекты, governance и видимость токенов уже завязаны на workspace ID.',
    },
  },
  workspace: {
    eyebrow: 'Контекст workspace',
    description: 'Scoped API guidance для {slug}.',
  },
  launchpad: {
    cards: {
      endpoint: 'Backend URL',
      endpointHint: 'Базовый endpoint текущей frontend-сессии.',
      auth: 'Session auth',
      authConnected: 'Bearer token сохранён',
      authMissing: 'Bearer token отсутствует',
      authHintReady: 'Хранится в browser session как {token}.',
      authHintMissing: 'Вставьте token ниже, чтобы открыть auth-required examples и inventory.',
      workspace: 'Workspace scope',
      workspaceMissing: 'Workspace не выбран',
      workspaceHintReady: 'Текущие examples уже завязаны на {workspace}.',
      workspaceHintMissing: 'Выберите workspace, чтобы получить scoped routes.',
      project: 'Project scope',
      projectWorkspaceWide: 'Режим всего workspace',
      projectHintReady: 'Query и document examples сейчас идут в {project}.',
      projectHintMissing: 'Можно остаться в workspace-wide режиме или выбрать проект глубже.',
    },
  },
  start: {
    eyebrow: 'С чего начать',
    title: 'Реалистичные первые шаги',
    description:
      'Страница честно показывает текущее состояние: публичные discovery routes работают сразу, auth-required routes требуют реальный bearer token, а minting токенов пока происходит вне этого UI.',
    status: {
      ready: 'Готово',
      saved: 'Сохранено',
      needsAction: 'Нужно действие',
      scoped: 'Есть scope',
      needsSetup: 'Нужна настройка',
      live: 'Можно запускать',
      waiting: 'Ожидание',
    },
    actions: {
      setup: 'Настроить workspace',
      ingest: 'Подготовить контент',
    },
    steps: {
      endpoint: {
        title: 'Проверьте backend URL',
        description: 'Используйте этот base URL для первых curl-вызовов и будущих docs links.',
      },
      token: {
        title: 'Подключите реальный bearer token',
        description:
          'UI может переиспользовать уже существующий token через browser session storage, но не притворяется, что умеет выпускать его сам.',
        hintReady: 'Session token {token} готов для protected reads.',
        hintMissing:
          'Сначала принесите token из operator/bootstrap path, затем пробуйте protected routes.',
      },
      scope: {
        title: 'Выберите workspace и project scope',
        description:
          'Examples становятся полезнее, когда в них есть реальный workspace ID и опциональный project ID.',
        descriptionReady: 'Текущие examples уже привязаны к {workspace}.',
        hintProject: 'Project-specific examples сейчас идут в {project}.',
        hintWorkspace: 'Workspace-wide режим остаётся полезным для discovery и admin tooling.',
      },
      requests: {
        title: 'Сначала public, потом protected',
        description:
          'Начните с health и project discovery, затем переходите к governance или query, когда auth уже на месте.',
        hintWithToken: 'Protected examples ниже уже можно вставлять в shell.',
        hintWithoutToken: 'Public examples ниже работают и без token.',
      },
    },
  },
  session: {
    eyebrow: 'Session auth',
    title: 'Session bearer token',
    description:
      'Сохраните token только на время этой browser session, чтобы protected product flows работали с реальным bearer token без притворства, что auth уже полноценно продуктован.',
    label: 'Bearer token',
    placeholder: 'rtrg_xxx_replace_me',
    activeLabel: 'Активный session token',
    activeNone: 'Token не сохранён',
    activeDescription:
      'Protected API reads могут использовать сохранённый bearer token, пока жива эта browser session.',
    missingDescription:
      'Public routes продолжают работать. Protected routes специально остаются закрытыми, пока token не задан.',
    actions: {
      save: 'Сохранить token',
      clear: 'Очистить token',
    },
    feedback: {
      saved: 'Session token сохранён для этой browser session.',
      cleared: 'Session token удалён из этой browser session.',
    },
    bootstrap: {
      label: 'Bootstrap secret',
      placeholder: 'RUSTRAG_BOOTSTRAP_TOKEN',
      action: 'Выпустить session token',
      actionBusy: 'Выпускаем...',
      hint: 'Используйте backend bootstrap secret только для первичного setup. Он выпускает instance-admin session token и сохраняет его локально для workspace, project, ingestion и query вызовов.',
      missingSecret: 'Сначала вставьте backend bootstrap secret.',
      success: 'Session token выпущен и сохранён для этой browser session.',
      rejected: 'Backend отклонил bootstrap secret.',
      notConfigured: 'На этом backend bootstrap token minting не настроен.',
    },
    status: {
      needsToken: 'Нужен token',
      connected: 'Подключено',
      limited: 'Ограниченный доступ',
      verifying: 'Проверяем доступ',
      needsCheck: 'Нужна проверка',
    },
    readiness: {
      governance: 'Workspace governance summary',
      tokens: 'Инвентарь workspace tokens',
      ready: 'Готово',
      needsToken: 'Нужен token',
      unauthorized: 'Не хватает scope',
      error: 'Проверка не прошла',
    },
    notes: {
      sessionOnly:
        'Хранение token здесь ограничено одной browser session и подходит только как локальное testing convenience.',
      mintingOutsideUi:
        'Bootstrap minting доступен, если backend отдает bootstrap secret flow, но дальнейший lifecycle token и раздача секретов всё ещё остаются задачей оператора.',
      plaintextOnce:
        'Plaintext token возвращается только в момент minting и не должен появляться здесь повторно.',
    },
  },
  inventory: {
    eyebrow: 'Текущий surface',
    title: 'Backend-backed inventory',
    description:
      'Публичные и auth-required surfaces разделены, поэтому страница остаётся полезной ещё до полного auth flow.',
    ready: 'Protected surface виден',
    needsToken: 'Режим только public',
    technicalSummary: 'Показать технический readiness checklist',
    technicalHint:
      'Foundation-checks и governance errors остаются доступны здесь, но больше не доминируют на странице.',
    cards: {
      workspaces: 'Workspaces',
      workspacesHint: 'Публичный список из /v1/workspaces.',
      projects: 'Проекты',
      projectsHint: 'Project scoping доступен без auth для discovery.',
      tokens: 'API tokens',
      tokensHint: 'Token summaries видны из /v1/auth/tokens, если хватает scope.',
      tokensPendingHint: 'Нужен bearer token с доступом к token inventory.',
      providerAccounts: 'Provider accounts',
      providerAccountsHint: 'Счётчик из workspace governance.',
      modelProfiles: 'Model profiles',
      modelProfilesHint: 'Счётчик из workspace governance.',
      protectedPendingHint: 'Появится после успешных protected workspace reads.',
    },
  },
  foundation: {
    workspaceContext: 'Workspace выбран для scoped API examples',
    sessionToken: 'Session token сохранён для protected API reads',
    projectScope: 'Есть хотя бы один project для scoped requests',
    protectedReadiness: 'Хотя бы один protected surface reachable',
    ready: 'Готово',
    todo: 'Нужна настройка',
  },
  tokens: {
    eyebrow: 'Инвентарь токенов',
    title: 'Инвентарь workspace tokens',
    description:
      'Здесь токены остаются read-only. Страница показывает label, scopes и свежесть, но не притворяется полноценным UX для minting или повторного показа секретов.',
    available: 'Инвентарь доступен',
    emptyBadge: 'Нет токенов',
    createdAt: 'Создан',
    lastUsedAt: 'Последнее использование',
    never: 'Никогда',
    missingTokenTitle: 'Сначала добавьте session token',
    missingTokenMessage:
      'Инвентарь токенов защищён, поэтому launchpad не сможет показать его, пока вы не передадите реальный bearer token.',
    missingTokenHint:
      'Используйте панель session auth выше после выпуска token в другом operator/bootstrap flow.',
    unauthorizedTitle: 'Не хватает scope для token inventory',
    unauthorizedMessage:
      'Сохранённый bearer token дошёл до backend, но его прав недостаточно, чтобы показать workspace tokens.',
    unauthorizedHint:
      'Используйте token с workspace-admin возможностями, если хотите видеть token inventory на этой странице.',
    errorTitle: 'Проверка token inventory не прошла',
    emptyTitle: 'API токенов пока нет',
    emptyMessage:
      'Protected route доступен, но backend не вернул видимых токенов для этого workspace.',
    emptyHint:
      'Backend умеет выпускать токены, но эта страница специально не притворяется полноценным secret-management flow.',
    scopeInventoryTitle: 'Инвентарь scopes',
    scopeInventoryDescription: 'Агрегированные scopes по видимым токенам.',
  },
  examples: {
    eyebrow: 'Первые запросы',
    title: 'Copy-pasteable first calls',
    description:
      'Примеры специально разделены на public discovery routes и protected routes, которым нужен реальный bearer token.',
    liveSurface: 'Текущий runtime surface',
    access: {
      public: 'Public route',
      token: 'Нужен token',
    },
    shared: {
      tokenExport:
        'Экспортируйте реальный token в `RUSTRAG_TOKEN`, прежде чем запускать protected calls.',
      workspaceScopeNote:
        'Держите workspace и project IDs согласованными, чтобы scoped calls оставались предсказуемыми и дебажными.',
    },
    cards: {
      health: {
        title: 'Проверить health сервиса',
        description:
          'Убедитесь, что backend поднят, прежде чем пробовать любые scoped integration flows.',
        note: 'Подходит для smoke tests, deploy checks и docs quickstarts.',
      },
      projects: {
        title: 'Получить список проектов workspace',
        description: 'Найдите проекты выбранного workspace до того, как выбирать query target.',
        note: 'Хороший первый вызов, когда известен только workspace ID.',
      },
      workspaceGovernance: {
        title: 'Прочитать workspace governance',
        description:
          'Получить counts по providers, profiles, tokens и usage для выбранного workspace.',
        note: 'Логичный следующий шаг после появления bearer auth.',
      },
      runQuery: {
        title: 'Запустить grounded query',
        description: 'Потрогать основной retrieval-and-answer path по выбранному проекту.',
        note: 'Это первый реалистичный product-facing API example после discovery.',
      },
    },
  },
  guidance: {
    eyebrow: 'Scoped guidance',
    title: 'Guidance по workspace и project',
    description:
      'Этот блок удерживает будущие API docs в реальности, привязывая examples к доступному сейчас workspace/project context.',
    projectScoped: 'Скоуп проекта',
    workspaceScoped: 'Скоуп workspace',
    allProjects: 'Вид по всему workspace',
    workspaceTitle: 'Интеграционная guidance для {workspace}',
    workspaceDescription:
      'Используйте этот режим для admin tooling или cross-project automation, когда на старте известен только workspace.',
    projectTitle: 'Интеграционная guidance для {project}',
    projectDescription:
      'Используйте {project}, когда нужны content, retrieval или document API в рамках одного проекта внутри {workspace}.',
    bullets: {
      scopeWorkspace:
        'Держите workspace slug ({workspace}) как человекочитаемый anchor в docs и operator tooling.',
      scopeProject:
        'Сохраняйте project slug ({project}) рядом с project ID, чтобы интеграции оставались дебажными.',
      tokenReuse:
        'Лучше переиспользовать внятно подписанный workspace token вроде «{token}», чем безымянные локальные секреты.',
      tokenMissing:
        'Видимого workspace token пока нет, значит внешним интеграциям всё ещё нужен отдельный token minting step.',
      permissionsScoped:
        'Неглобальный token уже есть, и это правильная база для project-scoped automation без лишнего instance-admin размаха.',
      permissionsAdminFallback:
        'Если сейчас у вас только instance-admin tokens, считайте их bootstrap credentials и планируйте даунскоуп позже.',
    },
  },
  groundwork: {
    eyebrow: 'Docs groundwork',
    title: 'Что уже подготовлено',
    description:
      'Теперь у product UI есть достаточная структура, чтобы достроиться до полноценных API docs, typed examples и auth walkthrough без выдумывания ещё неготовых возможностей.',
    status: {
      ready: 'Готово',
      next: 'Следующий слой',
    },
    cards: {
      contract: {
        title: 'OpenAPI как source of truth',
        description: 'Для runtime API surface уже существует hand-maintained контракт.',
        note: 'Держите контракт синхронным с backend routes, прежде чем публиковать более широкие docs.',
      },
      types: {
        title: 'Generated frontend types',
        description: 'Frontend contract types уже генерируются из OpenAPI документа.',
      },
      examples: {
        title: 'Структура scoped examples',
        description:
          'Страница теперь рендерит examples из живого workspace/project context вместо статичных placeholder docs.',
        note: 'Это удерживает будущие snippets привязанными к реальным IDs, scopes и backend URL.',
      },
      next: {
        title: 'Чего ещё не хватает',
        description:
          'Публичные docs, SDK examples и полноценный bootstrap auth walkthrough ещё не productized.',
        note: 'Эти куски нужно строить поверх contract pipeline и этого launchpad, а не фейковать внутри него.',
      },
    },
  },
  endpoints: {
    eyebrow: 'Известные endpoints',
    title: 'Сконфигурированные API endpoints',
    description:
      'Здесь показываются текущий base URL и самые полезные routes для выбранного workspace/project контекста.',
    configured: 'Endpoint настроен',
    unconfigured: 'Endpoint отсутствует',
  },
} as const
