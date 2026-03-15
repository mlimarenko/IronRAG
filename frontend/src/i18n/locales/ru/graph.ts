export default {
  page: {
    eyebrow: 'Граф знаний',
    title: 'Граф',
    description:
      'Проверьте готовность графа, ищите графовые сущности и смотрите, какие сущности и связи уже видны, а какие еще ждут поддержки backend.',
    statusLabel: 'Превью',
  },
  hero: {
    status: 'Превью',
    headline:
      'Graph помогает увидеть покрытие сущностей, видимость связей и evidence из retrieval по мере появления графовых данных.',
    body: 'У оператора уже есть понятная карта того, какие graph evidence доступны сейчас, что приходит из retrieval detail и какие записи графа пока зависят от поддержки backend.',
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
      message: 'Выберите concept из поиска, чтобы посмотреть доступные evidence и текущее покрытие backend.',
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
} as const
