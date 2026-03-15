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
        label: 'Technical graph',
        hint: 'Inspect graph coverage and relations.',
      },
      api: {
        label: 'Developer API',
        hint: 'Tokens, examples, and integration routes.',
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
        why: 'Use this technical view only when you need graph coverage, relation evidence, or readiness details beyond the main search workflow.',
        previous: 'Search stays the primary product path; Graph is a secondary inspection surface when answer-level grounding is not enough.',
        next: 'Move into the API surface only when you need deeper diagnostics, automation, or integration work.',
      },
      api: {
        stage: 'Integrate',
        why: 'Use this developer-facing area for tokens, examples, and automation once the main product flow is already working.',
        previous: 'Search and Files cover the core user journey first; API extends it for operators and developers.',
        next: 'Use this surface to extend the workflow outside the UI shell without crowding the primary product experience.',
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
      title: 'Technical graph',
      summary: 'Secondary graph inspection and readiness checks.',
    },
    api: {
      title: 'Developer API',
      summary: 'Secondary integration and automation tools.',
    },
  },
} as const
