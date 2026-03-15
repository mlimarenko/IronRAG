export default {
  brand: {
    title: 'RustRAG',
    subtitle: 'Операторская консоль',
  },
  nav: {
    product: 'Разделы продукта',
    groups: {
      flow: 'Основной поток',
      inspect: 'Проверка',
    },
    items: {
      processing: {
        label: 'Подготовка',
        hint: 'Выберите рабочий scope.',
      },
      files: {
        label: 'Файлы',
        hint: 'Добавляйте контент.',
      },
      search: {
        label: 'Поиск',
        hint: 'Проверяйте ответы по источникам.',
      },
      graph: {
        label: 'Граф',
        hint: 'Смотрите связи.',
      },
      api: {
        label: 'API',
        hint: 'Работайте с токенами и примерами.',
      },
    },
  },
  topbar: {
    surface: 'Текущий раздел',
    language: 'Язык',
    languageHint: 'Интерфейс',
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
      summary: 'Задайте активный scope.',
    },
    files: {
      title: 'Файлы',
      summary: 'Добавляйте контент.',
    },
    search: {
      title: 'Поиск',
      summary: 'Задавайте вопросы по библиотеке.',
    },
    graph: {
      title: 'Граф',
      summary: 'Проверяйте связи и доказательства.',
    },
    api: {
      title: 'API',
      summary: 'Подключайтесь к текущему API.',
    },
  },
} as const
