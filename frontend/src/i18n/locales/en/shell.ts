export default {
  brand: {
    title: 'RustRAG',
    subtitle: 'Operator console',
  },
  nav: {
    product: 'Product areas',
    items: {
      processing: {
        label: 'Processing',
        hint: 'Choose the space and library for this session.',
      },
      files: {
        label: 'Files',
        hint: 'Add notes and uploads to your library.',
      },
      search: {
        label: 'Search',
        hint: 'Ask questions and review grounded answers.',
      },
      graph: {
        label: 'Graph',
        hint: 'Inspect relationship coverage and evidence.',
      },
      api: {
        label: 'API',
        hint: 'Use tokens, examples, and endpoints.',
      },
    },
  },
  topbar: {
    surface: 'Current surface',
    language: 'Language',
    languageHint: 'Interface language',
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
      summary: 'Set the scope that powers files, search, graph, and API.',
    },
    files: {
      title: 'Files',
      summary: 'Bring new content into the active library.',
    },
    search: {
      title: 'Search',
      summary: 'Find answers grounded in your library.',
    },
    graph: {
      title: 'Graph',
      summary: 'See what relationships and evidence are visible today.',
    },
    api: {
      title: 'API',
      summary: 'Build against the live RustRAG API surface.',
    },
  },
} as const
