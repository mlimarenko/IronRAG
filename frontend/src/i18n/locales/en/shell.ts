export default {
  brand: {
    title: 'RustRAG',
    subtitle: 'RAG flow',
  },
  nav: {
    primary: 'Flow',
    manage: 'More',
    items: {
      files: {
        label: 'Files',
      },
      processing: {
        label: 'Overview',
      },
      ask: {
        label: 'Ask',
      },
      graph: {
        label: 'Graph',
      },
      api: {
        label: 'API',
      },
      context: {
        label: 'Setup',
      },
    },
  },
  topbar: {
    surface: 'Workspace',
    language: 'Language',
  },
  locale: {
    en: 'EN',
    ru: 'RU',
  },
  status: {
    focused: 'Focused',
    ready: 'Ready',
    healthy: 'Healthy',
  },
  pages: {
    files: {
      title: 'Files',
    },
    processing: {
      title: 'Overview',
    },
    context: {
      title: 'Setup',
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
