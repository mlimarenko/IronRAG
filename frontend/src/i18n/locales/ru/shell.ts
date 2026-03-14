export default {
  brand: {
    eyebrow: 'RustRAG',
    title: 'RustRAG',
    subtitle: 'Операторский shell для grounded content workflows.',
    badge: 'Preview',
  },
  nav: {
    primary: 'Product',
    manage: 'Context',
    items: {
      processing: {
        label: 'Processing',
        caption: 'Статус пайплайна',
      },
      files: {
        label: 'Files',
        caption: 'Индексированный контент',
      },
      ask: {
        label: 'Ask',
        caption: 'Grounded-ответы',
      },
      graph: {
        label: 'Graph',
        caption: 'Knowledge signals',
      },
      api: {
        label: 'API',
        caption: 'Integration surface',
      },
      context: {
        label: 'Context',
        caption: 'Workspace и project',
      },
    },
  },
  topbar: {
    surface: 'Surface',
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
    processing: {
      title: 'Processing',
    },
    context: {
      title: 'Context',
    },
    files: {
      title: 'Files',
    },
    ask: {
      title: 'Ask',
    },
    graph: {
      title: 'Graph',
    },
    api: {
      title: 'API',
    },
  },
} as const
