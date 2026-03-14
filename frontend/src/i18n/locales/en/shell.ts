export default {
  brand: {
    eyebrow: 'RustRAG',
    title: 'RustRAG',
    subtitle: 'Search and answer across your files.',
    badge: 'Preview',
  },
  nav: {
    primary: 'Explore',
    manage: 'Manage',
    items: {
      files: {
        label: 'Files',
        caption: 'Add and review indexed files',
      },
      processing: {
        label: 'Processing',
        caption: 'Status and next steps',
      },
      search: {
        label: 'Search',
        caption: 'Ask questions over your files',
      },
      graph: {
        label: 'Graph',
        caption: 'Explore connected knowledge',
      },
      api: {
        label: 'API',
        caption: 'Build on the same data',
      },
      setup: {
        label: 'Setup',
        caption: 'Choose your space and collection',
      },
    },
  },
  topbar: {
    surface: 'Section',
    language: 'Language',
    state: 'Status',
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
      title: 'Processing',
    },
    setup: {
      title: 'Setup',
    },
    search: {
      title: 'Search',
    },
    graph: {
      title: 'Graph',
    },
    api: {
      title: 'API',
    },
  },
} as const
