export default {
  brand: {
    title: 'RustRAG',
    subtitle: 'Операторская консоль',
  },
  nav: {
    product: 'Разделы продукта',
    items: {
      processing: {
        label: 'Подготовка',
        hint: 'Выберите пространство и библиотеку для текущей сессии.',
      },
      files: {
        label: 'Файлы',
        hint: 'Добавляйте заметки и загрузки в активную библиотеку.',
      },
      search: {
        label: 'Поиск',
        hint: 'Задавайте вопросы и проверяйте ответы с источниками.',
      },
      graph: {
        label: 'Граф',
        hint: 'Смотрите связи и доступные доказательства.',
      },
      api: {
        label: 'API Hub',
        hint: 'Начинайте с public routes, токенов и scoped examples.',
      },
    },
  },
  topbar: {
    surface: 'Текущая поверхность',
    language: 'Язык',
    languageHint: 'Язык интерфейса',
  },
  locale: {
    en: 'EN',
    ru: 'RU',
  },
  status: {
    focused: 'В фокусе',
    ready: 'Готово',
    healthy: 'Стабильно',
  },
  pages: {
    processing: {
      title: 'Подготовка',
      summary: 'Задайте scope, который используют Файлы, Поиск, Граф и API.',
    },
    files: {
      title: 'Файлы',
      summary: 'Добавляйте новый контент в активную библиотеку.',
    },
    search: {
      title: 'Поиск',
      summary: 'Ищите ответы по активной библиотеке.',
    },
    graph: {
      title: 'Граф',
      summary: 'Смотрите, какие связи и доказательства уже доступны.',
    },
    api: {
      title: 'API Hub',
      summary: 'Начинайте с живой API-поверхности RustRAG и достраивайте docs-ready usage.',
    },
  },
} as const
