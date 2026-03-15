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
      extend: 'Integrate',
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
        stage: 'Foundation',
        why: 'Choose the active workspace and library so every other surface shares the same operating context.',
        previous: 'Scope the session before you add content or inspect anything else.',
        next: 'Move into Files once the active library is set.',
      },
      files: {
        stage: 'Ingest',
        why: 'Bring content into the selected library and keep processing, inventory, and triage in one place.',
        previous: 'Processing defines which library receives new content.',
        next: 'Open Search once enough content is indexed.',
      },
      search: {
        stage: 'Operate',
        why: 'Ask grounded questions against the same library you prepared in Processing and fed in Files.',
        previous: 'Files determines what is actually searchable right now.',
        next: 'Use Graph when you need structure-level inspection beyond answer passages.',
      },
      graph: {
        stage: 'Inspect',
        why: 'Review entity coverage, relation evidence, and graph-readiness without losing the same project scope.',
        previous: 'Search shows answer-level grounding before you inspect graph structure directly.',
        next: 'Use API to carry this scoped context into integrations and automation.',
      },
      api: {
        stage: 'Integrate',
        why: 'Turn the same workspace and project context into repeatable API calls, tokens, and integration examples.',
        previous: 'Graph and Search clarify what the product is already surfacing before you automate it.',
        next: 'Use this surface to extend the workflow outside the UI shell.',
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
