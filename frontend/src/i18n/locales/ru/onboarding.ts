export default {
  title: 'Онбординг',
  description:
    'Настройте workspace, создайте project, подключите provider и загрузите первый документ, не выходя из консоли.',
  eyebrow: 'Пошаговая настройка',
  routeLabel: 'Онбординг',
  progressLabel: 'Прогресс',
  setupChecklistTitle: 'Чеклист настройки',
  setupChecklistHint:
    'Это реальный сценарий настройки. Шаг считается завершённым только после подтверждения от backend.',
  steps: {
    workspace: {
      title: 'Создать workspace',
      summary: 'Граница изоляции для проектов, provider account и политик.',
      action: 'Сохранить workspace',
      complete: 'Workspace создан',
    },
    project: {
      title: 'Создать project',
      summary: 'Основная RAG-поверхность, где сходятся документы, ingestion jobs и query.',
      action: 'Сохранить project',
      complete: 'Project создан',
    },
    provider: {
      title: 'Настроить provider',
      summary:
        'Создайте provider account и model profile, которые потом переиспользует query flow.',
      action: 'Сохранить provider setup',
      complete: 'Provider и profile сохранены',
    },
    document: {
      title: 'Загрузить первый документ',
      summary:
        'Создайте source, ingest plain text и при желании поставьте follow-up ingestion job.',
      action: 'Загрузить документ',
      complete: 'Документ загружен',
    },
  },
  fields: {
    workspaceName: 'Название workspace',
    workspaceSlug: 'Slug workspace',
    projectName: 'Название project',
    projectSlug: 'Slug project',
    projectDescription: 'Описание project',
    providerLabel: 'Label provider account',
    providerKind: 'Тип provider',
    apiBaseUrl: 'API base URL',
    profileKind: 'Тип profile',
    modelName: 'Имя модели',
    sourceLabel: 'Label source',
    sourceKind: 'Тип source',
    externalKey: 'External key',
    documentTitle: 'Название документа',
    documentText: 'Текст документа',
    queueJob: 'Поставить follow-up ingestion job в очередь',
  },
  hints: {
    slug: 'Для backend id лучше использовать lowercase, цифры и дефисы.',
    provider:
      'Используйте OpenAI/DeepSeek-compatible настройки, которые совпадают с backend environment.',
    document:
      'Здесь вызывается реальный text-ingestion endpoint. Фейковых completed-состояний нет.',
  },
  statuses: {
    pending: 'Ожидает',
    active: 'В процессе',
    complete: 'Готово',
    blocked: 'Заблокировано',
    attention: 'Нужно внимание',
  },
  cards: {
    currentState: 'Текущее состояние настройки',
    readiness: 'Готовность project после онбординга',
    connectedResources: 'Подключённые ресурсы',
    recentResult: 'Последний результат backend',
  },
  empty: {
    noWorkspace: 'Создайте workspace, чтобы открыть остальные шаги.',
    noProject: 'Создайте project перед настройкой provider и документа.',
    noProvider: 'Создайте provider account и model profile для этого workspace.',
    noDocument: 'Загрузите первый документ, чтобы project стал query-ready.',
  },
  actions: {
    refresh: 'Обновить состояние',
    openProjects: 'Открыть projects',
    openProviders: 'Открыть providers',
    openIngestion: 'Открыть ingestion',
  },
  metrics: {
    workspaces: 'Workspaces',
    projects: 'Projects',
    providerAccounts: 'Provider accounts',
    modelProfiles: 'Model profiles',
    documents: 'Documents',
    jobs: 'Jobs',
    sources: 'Sources',
    readiness: 'Readiness',
    indexingState: 'Indexing state',
  },
  messages: {
    loaded: 'Состояние онбординга загружено.',
    workspaceCreated: 'Workspace успешно сохранён.',
    projectCreated: 'Project успешно сохранён.',
    providerCreated: 'Provider setup успешно сохранён.',
    documentCreated: 'Документ успешно загружен.',
    refreshFailed: 'Не удалось обновить состояние онбординга.',
  },
} as const
