export default {
  eyebrow: 'Админский поток',
  title: 'Провайдеры',
  description:
    'Подготовка аккаунтов провайдеров и дефолтов профилей моделей для каждого workspace без видимости, будто backend уже умеет полноценные сценарии назначения.',
  providerAccounts: 'Аккаунты провайдеров',
  modelProfiles: 'Профили моделей',
  providerKinds: {
    openai: 'OpenAI',
    deepseek: 'DeepSeek',
    compatible: 'Совместимый API',
  },
  states: {
    loading: 'Загрузка',
    blocked: 'Блокер',
    attention: 'Нужно внимание',
    ready: 'Готово',
    idle: 'Ожидание',
    configured: 'Настроено',
    missing: 'Отсутствует',
    incomplete: 'Неполно',
    available: 'Доступно',
    seeded: 'Подготовлено',
    pending: 'В очереди',
    suggested: 'Рекомендуется',
    todo: 'Нужна настройка',
  },
  actions: {
    refresh: 'Обновить governance',
  },
  loading: {
    title: 'Загрузка governance провайдеров',
  },
  errors: {
    governanceUnavailableTitle: 'Сводка по провайдерам недоступна',
    unknown: 'Неизвестная ошибка провайдеров',
  },
  empty: {
    noWorkspaceTitle: 'Workspace не выбран',
    noWorkspaceMessage: 'Для показа сценария настройки провайдеров нужен workspace.',
    noWorkspaceHint:
      'Когда CRUD для workspace будет полностью подключен, эта страница сохранит тот же layout и просто получит реальные create/edit потоки.',
  },
  workspaceStrip: {
    eyebrow: 'Контекст workspace',
    description: 'Подсказки по настройке провайдеров для {slug}.',
  },
  wizard: {
    eyebrow: 'Настройка провайдеров',
    title: 'Мастер настройки провайдеров',
    description:
      'Этот пошаговый сценарий помогает оператору пройти настройку провайдеров и честно показывает, что платформа уже умеет сохранять, а что ещё требует backend-поддержки.',
    providerKindsTitle: 'Выберите семейство провайдера',
    providerKindsDescription:
      'Здесь можно решить, какой аккаунт провайдера стоит завести первым для выбранного workspace.',
    profileKindsTitle: 'Выберите семейство профиля',
    profileKindsDescription:
      'Семейства профилей повторяют текущие backend kinds и специально остаются узкими.',
    nextActionTitle: 'Следующий шаг',
    nextActionProvider: 'Создайте первый аккаунт {provider} для этого workspace.',
    nextActionProfile:
      'Добавьте профиль модели типа {profile}, когда аккаунт провайдера уже создан.',
    nextActionAssignments:
      'Проверьте рекомендуемые профили по умолчанию. Полноценные назначения на уровне проекта ещё требуют backend-поддержки.',
    honestyNote:
      'Текущие возможности backend: просмотр и создание provider accounts, просмотр и создание model profiles, а также governance summary.',
    steps: {
      providerAccount: {
        title: 'Аккаунт провайдера',
        description: 'Начните с записи API-аккаунта, привязанной к workspace.',
      },
      modelProfile: {
        title: 'Профиль модели',
        description: 'Привяжите chat, embedding или rerank профиль к аккаунту провайдера.',
      },
      assignments: {
        title: 'Рекомендуемые дефолты',
        description: 'Предпросмотр вероятных связок до появления настоящего assignment UX.',
      },
    },
    providerKinds: {
      openai: {
        label: 'Сначала OpenAI',
        helper: 'Хороший дефолт, если быстро нужны и chat, и embeddings.',
      },
      deepseek: {
        label: 'Сначала DeepSeek',
        helper: 'Подходит для экономичных chat-сценариев, если команда уже пользуется DeepSeek.',
      },
      compatible: {
        label: 'Совместимый endpoint',
        helper: 'Оставьте для self-hosted или OpenAI-compatible шлюзов.',
      },
    },
  },
  accounts: {
    eyebrow: 'Инвентарь аккаунтов',
    description: 'Аккаунты провайдеров workspace, которые сейчас реально возвращает backend.',
    emptyTitle: 'Аккаунтов провайдеров пока нет',
    emptyMessage: 'Для этого workspace ещё нет аккаунта {provider}.',
    emptyHint:
      'Ввод секретов, проверка base URL и тестирование учётных данных всё ещё требуют отдельной поддержки в backend/API.',
  },
  profiles: {
    eyebrow: 'Инвентарь профилей',
    description:
      'Профили моделей сгруппированы по поддерживаемым backend kinds, чтобы следующие пикеры опирались на ту же структуру.',
    emptyTitle: 'Профилей моделей пока нет',
    emptyMessage: 'Создайте хотя бы один профиль модели после появления аккаунта провайдера.',
    emptyHint:
      'Temperature, token limits и capability metadata есть на уровне создания, но текущий UI пока честно остаётся read-focused.',
    groupEmptyTitle: 'Профилей {profile} пока нет',
    groupEmptyMessage: 'В этом workspace пока не настроен профиль типа {profile}.',
    kinds: {
      chat: {
        label: 'Chat',
        helper: 'Основной генеративный профиль для синтеза ответа.',
      },
      embedding: {
        label: 'Embedding',
        helper: 'Профиль векторизации для индексации и retrieval.',
      },
      rerank: {
        label: 'Rerank',
        helper: 'Необязательный профиль пересортировки для более точного retrieval.',
      },
    },
  },
  recommendations: {
    eyebrow: 'Рекомендации профилей по умолчанию',
    title: 'Рекомендуемые профили по умолчанию',
    description:
      'Эти карточки задают будущий layout пикера: по одному рекомендованному профилю на capability внутри выбранного workspace.',
    missingTitle: 'Для {profile} пока нет рекомендации',
    missingMessage:
      'Создайте профиль модели типа {profile}, чтобы здесь появилась рекомендация по умолчанию.',
  },
  summary: {
    providerAccounts: 'Аккаунты провайдеров',
    modelProfiles: 'Профили моделей',
    recommendedPairing: 'Базовый рекомендуемый аккаунт',
    providerAccountsHintReady: 'Есть как минимум один аккаунт провайдера.',
    providerAccountsHintEmpty: 'Настройка всё ещё стартует с создания аккаунта.',
    modelProfilesHintReady: 'Профили уже есть и смогут питать будущие пикеры.',
    modelProfilesHintEmpty: 'Пока нельзя вывести дефолтные профили.',
    recommendedPairingInferred: 'Выведено из текущего инвентаря',
    recommendedPairingMissing: 'Нечего рекомендовать',
    recommendedPairingHint:
      'Первый подходящий аккаунт провайдера становится текущей рекомендацией.',
  },
} as const
