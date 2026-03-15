export default {
  brand: {
    title: 'RustRAG',
    subtitle: 'Operator console',
  },
  nav: {
    product: 'Product areas',
    groups: {
      flow: 'Core flow',
      inspect: 'Inspect',
    },
    items: {
      processing: {
        label: 'Processing',
        hint: 'Choose session scope.',
      },
      files: {
        label: 'Files',
        hint: 'Add content.',
      },
      search: {
        label: 'Search',
        hint: 'Review grounded answers.',
      },
      graph: {
        label: 'Graph',
        hint: 'Inspect relationships.',
      },
      api: {
        label: 'API',
        hint: 'Use tokens and examples.',
      },
    },
  },
  topbar: {
    surface: 'Current area',
    language: 'Language',
    languageHint: 'Interface',
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
      summary: 'Set the active scope.',
    },
    files: {
      title: 'Files',
      summary: 'Add content.',
    },
    search: {
      title: 'Search',
      summary: 'Ask grounded questions.',
    },
    graph: {
      title: 'Graph',
      summary: 'Inspect relations and evidence.',
    },
    api: {
      title: 'API',
      summary: 'Build against the live API.',
    },
  },
} as const
