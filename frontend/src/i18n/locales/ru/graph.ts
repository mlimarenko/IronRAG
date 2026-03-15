export default {
  page: {
    eyebrow: 'Поддерживающий graph-контекст',
    title: 'Граф',
    description:
      'Используйте эту вторичную поверхность только когда Ask нужен дополнительный graph-контекст, поиск связей или проверка readiness.',
    technicalNote:
      'Это вторичная техническая поверхность для проверки graph coverage и readiness. Для основного сценария ответов оставайтесь в Поиске.',
    statusLabel: 'Превью',
  },
  hero: {
    status: 'Превью',
    headline:
      'Используйте Graph только как supporting-контекст, когда основному Ask-сценарию нужны дополнительные evidence по сущностям или связям.',
    body: 'Эта поверхность помогает проверить graph coverage, retrieval-linked evidence и backend gaps, не заменяя Ask как главный способ задавать вопросы.',
    highlights: [
      'В retrieval detail уже есть ссылки, совпавшие чанки и сырые debug-данные.',
      'Панели поиска и деталей явно показывают, какие graph-данные доступны прямо сейчас.',
      'Когда появятся отдельные graph API, эта страница сможет перейти от объяснений к живому просмотру сущностей и связей без смены сценария.',
    ],
  },
  metrics: {
    entityCoverage: {
      label: 'Покрытие сущностей',
      value: 'Ждем первый retrieval run',
    },
    relationFreshness: {
      label: 'Актуальность связей',
      value: 'Снимков графа пока нет',
    },
    operatorPosture: {
      label: 'Готовность оператора',
      value: 'Workspace готов к graph signals',
    },
  },
  summary: {
    eyebrow: 'Текущее покрытие',
    title: 'Сводка по графу',
    description:
      'Что RustRAG умеет показать уже сейчас, что приходит из retrieval, и где graph records со стороны backend пока отсутствуют.',
    status: 'Превью',
    rows: {
      sourceOfTruth: {
        label: 'Текущий источник правды',
        value: 'Детали retrieval run и graph-backed metadata',
      },
      availableEvidence: {
        label: 'Доступные graph evidence',
        value: 'Ссылки, совпавшие чанки, debug-данные',
      },
      unavailable: {
        label: 'Пока недоступно',
        value: 'Список сущностей, связи, graph API с provenance',
      },
    },
  },
  search: {
    eyebrow: 'Поиск',
    title: 'Поиск по графу',
    description:
      'Ищите по доступным graph signals и возможностям backend, которых этому workspace пока не хватает.',
    label: 'Искать graph concepts',
    placeholder: 'Ищите сущности, связи, retrieval, debug...',
    empty: {
      title: 'Совпадений в графе пока нет',
      message: 'По этому запросу на странице пока нет подходящих graph concepts.',
      hint: 'Попробуйте более общие термины вроде retrieval, relation, entity или debug. Поиск станет богаче по мере появления graph API и индексированных данных.',
    },
    results: {
      retrievalSignals: {
        title: 'Сигналы retrieval',
        kind: 'Доступно сейчас',
        summary:
          'Сейчас все данные рядом с графом приходят из retrieval references, matched chunks и debug JSON, сохраненного для каждого запуска.',
        evidence: ['references[]', 'matched_chunk_ids[]', 'debug_json'],
      },
      entityIndex: {
        title: 'Индекс сущностей',
        kind: 'Ждет данные от backend',
        summary:
          'Интерфейс поиска готов показывать сущности, но backend пока не отдает канонический список сущностей.',
        evidence: ['Нужен API списка сущностей', 'Нужна project-scoped индексация'],
      },
      relationInspector: {
        title: 'Просмотр связей',
        kind: 'Ждет данные от backend',
        summary:
          'Детали связей готовы для оператора, но платформа пока не возвращает relation tuples.',
        evidence: ['Нужен API связей', 'Нужен provenance payload'],
      },
    },
  },
  detail: {
    eyebrow: 'Детали',
    title: 'Детали графа',
    description:
      'Посмотрите выбранный concept, какие evidence доступны сейчас и можно ли уже проверить живые relation records.',
    waitingOnApi: 'Ждем API',
    evidenceTitle: 'Доступные evidence',
    relationTitle: 'Вид связей',
    emptyRelations: {
      title: 'Живых связей пока нет',
      message: 'Backend пока не отдает канонические relation tuples для этого concept.',
      hint: 'Как только graph API начнут отдавать связи, здесь появятся edges и соседние узлы с provenance вместо поясняющего текста.',
    },
    emptySelection: {
      title: 'Детали графа не выбраны',
      message:
        'Выберите concept из поиска, чтобы посмотреть доступные evidence и текущее покрытие backend.',
      hint: 'Так страница остается полезной и не выдумывает сущности или связи, которых пока не существует.',
    },
    relations: {
      retrievalSignals: [
        {
          from: 'Retrieval run',
          relation: 'фиксирует',
          to: 'Ссылки',
        },
        {
          from: 'Retrieval run',
          relation: 'совпадает с',
          to: 'ID чанков',
        },
        {
          from: 'Retrieval run',
          relation: 'сохраняет',
          to: 'Debug payload',
        },
      ],
    },
  },
  states: {
    chooseProject: 'Выберите проект',
    loadingSurface: 'Загружаем граф',
    backendPending: 'Точка входа backend еще не готова',
    surfaceDegraded: 'Поверхность графа деградировала',
  },
  actions: {
    processing: 'Настроить scope',
    ingest: 'Загрузить контент',
  },
  surface: {
    noProject: {
      status: 'Заблокировано',
      headline: 'Выберите проект, чтобы проверить связи графа.',
      body: 'Этот экран готов показывать сохраненные сущности и покрытие связей сразу после выбора project scope.',
      highlights: [
        'Project scope приходит из того же workspace flow, который используют Файлы и Поиск.',
        'Страница явно показывает отсутствие контекста и не выдумывает graph-данные.',
        'Как только проект выбран, экран сразу пробует живые graph endpoint-ы.',
      ],
    },
    unavailable: {
      status: 'Точка входа готова',
      headline:
        'UI для графа готов, но эта сборка backend пока не отдает runtime routes для графа.',
      body: 'Продуктовая поверхность уже project-scoped и готова к реальным graph-данным, но `/graph-products/*` все еще требует wiring в текущем окружении.',
      highlights: [
        'При недоступном маршруте UI не рисует фейковые сущности и связи.',
        'Выбор проекта, статусы и empty states уже готовы для продукта.',
        'Как только backend отдаст graph routes, эта же страница автоматически загорится живыми данными.',
      ],
    },
    live: {
      status: 'Живые graph rows',
      headline:
        'Проверяйте сохраненные сущности, покрытие связей и результаты поиска для выбранного проекта.',
      body: 'Эта страница читает реальные graph rows. Поиск по связям и детали сущностей работают там, где backend уже сохранил записи.',
      highlights: [
        'Результаты поиска строятся по сохраненным сущностям и relation rows, а не по placeholder-тексту.',
        'Детали сущности показывают алиасы, документы, ссылки на чанки и наблюдаемые связи.',
        'Предупреждения остаются видимыми, если tracking extraction или глубина provenance еще частичны.',
      ],
    },
    waiting: {
      status: 'Ждем extraction',
      headline: 'Graph endpoints отвечают, но у этого проекта пока нет сохраненных relation rows.',
      body: 'Экран уже работает против backend, а текущий блокер в том, что runtime extraction еще не заполнил entity и relation rows для этого проекта.',
      highlights: [
        'Страница подтверждает достижимость backend даже когда graph counts равны нулю.',
        'Счетчики сущностей и связей останутся нулевыми, пока extraction не запишет строки.',
        'Как только строки появятся, поиск и панель деталей переключатся на живые данные без изменений в UI.',
      ],
    },
  },
  metricLabels: {
    entities: 'Сущности',
    relations: 'Связи',
    extractionRuns: 'Запуски extraction',
    noProjectSelected: 'Проект не выбран',
    awaitingProjectScope: 'Ждем scope проекта',
    backendRoutePending: 'Маршрут backend еще не готов',
  },
  panels: {
    summary: {
      eyebrow: 'Scope и готовность',
      title: 'Сводка по графу',
      description:
        'Project-scoped готовность графа, живое покрытие и текущее ограничение, которое пока мешает полноценно извлекать связи.',
      workspace: 'Workspace',
      workspaceEmpty: 'Workspace не выбран',
      project: 'Проект',
      projectPlaceholder: 'Выберите проект',
      relationKinds: 'Типы связей',
      entityKinds: 'Типы сущностей',
      currentBlocker: 'Текущий блокер',
      blockerApiUnavailable: 'Маршрут backend в этой runtime-сборке еще не подключен.',
      blockerPartial: 'Трекинг extraction и глубина provenance пока частичные.',
      blockerNoRows: 'Runtime extraction еще не записал entity и relation rows для этого проекта.',
    },
    search: {
      eyebrow: 'Поиск',
      title: 'Поиск по графу',
      description:
        'Ищите сохраненные сущности и связи, когда graph runtime доступен. Без запроса панель показывает top entities и sample relations.',
      label: 'Искать graph concepts',
      placeholder: 'Ищите сущности, связи, алиасы...',
      loading: 'Загружаем граф',
      noProject: {
        title: 'Сначала выберите проект',
        message:
          'Граф привязан к проекту. Выберите проект, чтобы посмотреть покрытие сущностей и связей.',
        hint: 'Селектор в этой панели использует тот же session scope, что и остальная shell-навигация.',
      },
      unavailable: {
        title: 'Маршрут graph backend недоступен',
        message:
          'Эта продуктовая поверхность готова, но текущий backend еще не отдает `/graph-products/*`.',
        hint: 'Подключение backend остается последним блокером перед появлением живых данных по сущностям и связям.',
      },
      noMatches: {
        title: 'Совпадений в графе пока нет',
        message: 'Ни одна сохраненная сущность или связь не совпала с этим запросом.',
        hint: 'Попробуйте более общий canonical name, alias или relation type.',
      },
      noRows: {
        title: 'Graph rows пока нет',
        message: 'У этого проекта пока нет сохраненных graph rows.',
        hint: 'Как только extraction запишет entity и relation rows, панель поиска заполнится автоматически.',
      },
      searching: 'Ищем graph records...',
    },
    detail: {
      eyebrow: 'Детали',
      title: 'Детали графа',
      description:
        'Проверяйте сохраненные evidence по сущности, bounded subgraph соседей и покрытие связей, не выдумывая provenance, которого backend пока не отдает.',
      loading: 'Загружаем детали',
      loadErrorTitle: 'Не удалось загрузить детали сущности',
      loadErrorHint:
        'Покрытие и результаты поиска все равно можно проверить, пока backend-детали этой сущности расследуются.',
      emptySelection: {
        title: 'Детали графа не выбраны',
        message:
          'Выберите сущность или связь из панели поиска, чтобы посмотреть живое покрытие графа.',
        hint: 'Панель деталей рендерит только сохраненные graph-данные и явные блокеры.',
      },
      technicalSummary: 'Показать технические настройки графа',
      technicalHint:
        'Меняйте глубину subgraph и проверяйте ограниченную graph-структуру только когда это действительно нужно.',
      subgraphSummary: 'Показать bounded subgraph и структуру связей',
      subgraphDepth: 'Глубина subgraph',
      entitySummary: 'С этой сущностью связано {count} наблюдаемых связей.',
      aliases: 'Алиасы',
      noAliases: 'Для этой сущности не сохранено ни одного алиаса.',
      documents: 'Source document ids',
      noDocuments: 'Для этой сущности source document ids пока не сохранены.',
      chunks: 'Source chunk ids',
      noChunks: 'Для этой сущности source chunk ids пока не сохранены.',
      subgraphEyebrow: 'Subgraph',
      subgraphTitle: 'Окрестность {name}',
      subgraphStats: '{entities} сущностей · {relations} связей',
      subgraphHint: 'Ограниченное graph expansion сейчас загружено с глубиной {depth}.',
      subgraphEntities: 'Сущности в subgraph',
      noSubgraphEntities: 'Для этого bounded subgraph соседние сущности не вернулись.',
      subgraphRelations: 'Связи в subgraph',
      noSubgraphRelations: 'Для этого bounded subgraph сохраненные связи пока не вернулись.',
      outgoingRelations: 'Исходящие связи',
      noOutgoingRelations: 'У этой сущности сейчас нет исходящих сохраненных связей.',
      incomingRelations: 'Входящие связи',
      noIncomingRelations: 'У этой сущности сейчас нет входящих сохраненных связей.',
      matchReasons: 'Причины совпадения',
      noMatchReasons: 'Эта запись пришла из live summary, а не из query-specific поиска.',
    },
    diagnostics: {
      eyebrow: 'Диагностика',
      title: 'Диагностика графа',
      description:
        'Смотрите живые content counts, покрытие provenance, blockers готовности и следующий видимый оператору шаг по проекту.',
      pending: 'Ждем диагностику',
      loading: 'Загружаем диагностику',
      noProject: {
        title: 'Сначала выберите проект',
        message:
          'Диагностика привязана к проекту. Выберите его, чтобы увидеть готовность графа и блокеры.',
        hint: 'Тот же выбранный проект управляет graph search, detail и diagnostics.',
      },
      unavailable: {
        title: 'Маршрут graph diagnostics недоступен',
        message: 'Этот runtime пока не отдает graph diagnostics.',
        hint: 'Когда backend-маршрут появится, здесь будут counts контента, gaps provenance и next steps.',
      },
      metrics: {
        documents: 'Сохраненные документы',
        chunks: 'Сохраненные чанки',
        embeddings: 'Эмбеддинги чанков',
        retrievalRuns: 'Retrieval runs',
        entityRefs: 'Сущности с chunk refs',
        relationRefs: 'Связи с chunk refs',
      },
      blockersTitle: 'Текущие блокеры',
      noBlockers: 'Явные блокеры не вернулись.',
      nextStepsTitle: 'Следующие шаги',
      noNextSteps: 'Следующие шаги не вернулись.',
      technicalSummary: 'Показать технические метрики покрытия',
      technicalHint:
        'Counts по документам, чанкам, эмбеддингам и provenance остаются доступны здесь как вторичная диагностика.',
    },
  },
  common: {
    noGraphRows: 'Graph rows пока нет',
  },
  errors: {
    loadEntityDetail: 'Не удалось загрузить детали сущности',
    loadPageContext: 'Не удалось загрузить контекст graph page',
    loadCoverage: 'Не удалось загрузить покрытие графа',
    searchFailed: 'Поиск по графу завершился ошибкой',
  },
} as const
