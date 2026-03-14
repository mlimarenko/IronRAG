export default {
  brand: {
    title: 'RustRAG',
    subtitle: 'RAG flow',
  },
  nav: {
    primary: 'Основное',
    manage: 'Ещё',
    items: {
      files: {
        label: 'Файлы',
      },
      processing: {
        label: 'Обзор',
      },
      ask: {
        label: 'Вопросы',
      },
      graph: {
        label: 'Граф',
      },
      api: {
        label: 'API',
      },
      context: {
        label: 'Настройка',
      },
    },
  },
  topbar: {
    surface: 'Workspace',
    language: 'Язык',
  },
  locale: {
    en: 'EN',
    ru: 'RU',
  },
  status: {
    focused: 'Фокус',
    ready: 'Готово',
    healthy: 'Стабильно',
  },
  pages: {
    files: {
      title: 'Файлы',
    },
    processing: {
      title: 'Обзор',
    },
    context: {
      title: 'Настройка',
    },
    ask: {
      title: 'Вопросы',
    },
    graph: {
      title: 'Граф',
    },
    api: {
      title: 'API',
    },
  },
} as const
