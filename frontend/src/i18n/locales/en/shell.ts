export default {
  brand: {
    title: 'RustRAG',
    subtitle: 'Home → Files → Ask',
  },
  nav: {
    product: 'Product areas',
    groups: {
      flow: 'Main flow',
      inspect: 'Inspect',
      extend: 'Integrate',
    },
    items: {
      home: {
        label: 'Home',
        hint: 'Start from the primary product path.',
      },
      processing: {
        label: 'Setup',
        hint: 'Pick your space and library.',
      },
      files: {
        label: 'Files',
        hint: 'Add your first content.',
      },
      search: {
        label: 'Ask',
        hint: 'Ask questions with sources.',
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
    surface: 'Current step',
    language: 'Language',
    languageHint: 'Interface',
  },
  mobileNav: {
    primary: 'Primary product navigation',
    more: 'More',
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
      home: {
        stage: 'Start',
        why: 'Home keeps the main product path visible first so setup, files, and asking stay easy to reach from mobile and desktop alike.',
        previous: 'This is the landing point for the product flow before you decide whether to set up, add files, or ask.',
        next: 'Move into Files when the active library is ready, or open Setup when you still need to pick the scope.',
      },
      processing: {
        stage: 'Step 1',
        why: 'Pick the space and library you want to work in before adding content or asking questions.',
        previous: 'Start here so the rest of the product knows where your content should live.',
        next: 'Move into Files once the active library is ready.',
      },
      files: {
        stage: 'Step 2',
        why: 'Add content to the selected library and keep progress visible in one place.',
        previous: 'Setup chooses the library that should receive new content.',
        next: 'Open Ask once enough content is indexed.',
      },
      search: {
        stage: 'Step 3',
        why: 'Ask questions against the same library you prepared in Setup and filled in Files.',
        previous: 'Files determines what is actually ready to answer questions.',
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
    home: {
      title: 'Home',
      summary: 'Start with the main product path.',
    },
    processing: {
      title: 'Setup',
      summary: 'Choose your library.',
    },
    files: {
      title: 'Files',
      summary: 'Add content.',
    },
    search: {
      title: 'Ask',
      summary: 'Ask questions with sources.',
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
