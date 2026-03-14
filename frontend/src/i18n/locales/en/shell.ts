export default {
  brand: {
    eyebrow: 'RustRAG',
    title: 'RustRAG',
    subtitle: 'Operator shell for grounded content workflows.',
    badge: 'Preview',
  },
  nav: {
    primary: 'Product',
    manage: 'Context',
    items: {
      processing: {
        label: 'Processing',
        caption: 'Pipeline status',
      },
      files: {
        label: 'Files',
        caption: 'Indexed content',
      },
      ask: {
        label: 'Ask',
        caption: 'Grounded answers',
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
        caption: 'Workspace and project',
      },
    },
  },
  topbar: {
    surface: 'Surface',
    language: 'Language',
    state: 'Runtime',
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
