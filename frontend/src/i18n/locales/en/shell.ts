export default {
  brand: {
    eyebrow: 'RustRAG',
    title: 'Product console',
    subtitle: 'A cleaner shell for workspace, content, and grounded search.',
    badge: 'Preview',
  },
  nav: {
    primary: 'Core',
    manage: 'Manage',
    items: {
      overview: {
        label: 'Overview',
        caption: 'Flow status',
      },
      workspace: {
        label: 'Workspace',
        caption: 'Context',
      },
      library: {
        label: 'Library',
        caption: 'Content',
      },
      search: {
        label: 'Search',
        caption: 'Answers',
      },
    },
  },
  topbar: {
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
    overview: {
      section: 'Overview',
      title: 'Minimal product flow',
    },
    workspace: {
      section: 'Workspace',
      title: 'Workspace and project context',
    },
    library: {
      section: 'Library',
      title: 'Content library',
    },
    search: {
      section: 'Search',
      title: 'Grounded answers',
    },
  },
} as const
