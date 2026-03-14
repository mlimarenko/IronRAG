export default {
  brand: {
    eyebrow: 'RustRAG',
    title: 'Продуктовая консоль',
    subtitle: 'Более чистый shell для workspace, контента и grounded search.',
    badge: 'Preview',
  },
  nav: {
    primary: 'Основа',
    manage: 'Управление',
    items: {
      overview: {
        label: 'Обзор',
        caption: 'Состояние потока',
      },
      workspace: {
        label: 'Workspace',
        caption: 'Контекст',
      },
      library: {
        label: 'Библиотека',
        caption: 'Контент',
      },
      search: {
        label: 'Поиск',
        caption: 'Ответы',
      },
    },
  },
  topbar: {
    language: 'Язык',
    state: 'Состояние',
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
    overview: {
      section: 'Обзор',
      title: 'Минимальный продуктовый поток',
    },
    workspace: {
      section: 'Workspace',
      title: 'Контекст workspace и project',
    },
    library: {
      section: 'Библиотека',
      title: 'Библиотека контента',
    },
    search: {
      section: 'Поиск',
      title: 'Grounded-ответы',
    },
  },
} as const
