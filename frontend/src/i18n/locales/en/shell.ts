export default {
  brand: {
    title: 'RustRAG',
    subtitle: 'Documents → Ask + Graph',
  },
  nav: {
    product: 'Product areas',
    groups: {
      primary: 'Primary flow',
      advanced: 'Advanced',
    },
    items: {
      processing: {
        label: 'Advanced setup',
        hint: 'Choose space, library, and operator access when needed.',
      },
      files: {
        label: 'Documents',
        hint: 'Upload documents, watch processing, and take the next action.',
      },
      search: {
        label: 'Ask + Graph',
        hint: 'Ask questions, review answers, and inspect related graph context.',
      },
      api: {
        label: 'Developer API',
        hint: 'Tokens, examples, and integration routes.',
      },
    },
  },
  topbar: {
    surface: 'Current step',
    language: 'Language',
    languageHint: 'Interface',
  },
  mobileNav: {
    primary: 'Primary product navigation',
    advanced: 'Advanced',
  },
  spine: {
    eyebrow: 'Product spine',
  },
  guide: {
    eyebrow: 'How this area fits',
    previous: 'Comes from',
    next: 'Leads to',
    related: 'Also connects to',
    start: 'This is the start of the product flow.',
    end: 'This is the furthest surface in the current shell.',
    sections: {
      processing: {
        stage: 'Advanced',
        why: 'Keep workspace, library, and access configuration available without letting setup own the primary path.',
        previous: 'Most people should arrive here only when they need to change scope or unlock access.',
        next: 'Return to Documents after adjusting the active library or permissions.',
      },
      files: {
        stage: 'Step 1',
        why: 'Documents is the main operational page: upload files, watch processing, review status, and move on when the library is ready.',
        previous: 'This is the main landing point for the product experience.',
        next: 'Open Ask + Graph once documents are indexed enough to answer honestly.',
      },
      search: {
        stage: 'Step 2',
        why: 'Ask + Graph keeps questions, answers, sources, and related graph context in one primary surface.',
        previous: 'Documents determines what is actually ready to answer questions.',
        next: 'Use advanced setup or API only when you need operator controls beyond the normal document workflow.',
      },
    },
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
      title: 'Advanced setup',
      summary: 'Change workspace, library, and access when needed.',
    },
    files: {
      title: 'Documents',
      summary: 'Upload documents, monitor processing, and keep next actions obvious.',
    },
    search: {
      title: 'Ask + Graph',
      summary: 'Ask questions, review answers, and inspect related graph context.',
    },
    graph: {
      title: 'Graph diagnostics',
      summary: 'Secondary graph inspection and evidence checks.',
    },
    api: {
      title: 'Developer API',
      summary: 'Secondary integration and automation tools.',
    },
  },
} as const
