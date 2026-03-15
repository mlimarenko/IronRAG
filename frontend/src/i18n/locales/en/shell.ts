export default {
  brand: {
    title: 'RustRAG',
    subtitle: 'Documents and Ask',
  },
  nav: {
    product: 'Product navigation',
    groups: {
      primary: 'Main',
      advanced: 'Advanced',
    },
    items: {
      documents: {
        label: 'Documents',
        hint: 'Upload documents, check progress, and see what is ready.',
      },
      ask: {
        label: 'Ask',
        hint: 'Ask questions and review answers with related context.',
      },
      advanced: {
        label: 'Advanced',
        hint: 'Workspace, library, and integration controls when needed.',
      },
    },
  },
  topbar: {
    surface: 'Current page',
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
    eyebrow: 'How this page fits',
    previous: 'Comes from',
    next: 'Leads to',
    related: 'Also available',
    start: 'This is the main starting point.',
    end: 'This is the last page in the main flow.',
    sections: {
      documents: {
        stage: 'Step 1',
        why: 'Upload documents, watch processing, and see when content is ready to ask about.',
        previous: 'This is the default front door for the app.',
        next: 'Open Ask when your content is ready or keep uploading.',
      },
      ask: {
        stage: 'Step 2',
        why: 'Ask questions, continue conversations, and inspect related context in one place.',
        previous: 'Documents tells you what is ready to ask about.',
        next: 'Use Advanced only when you need to change workspace, library, or integrations.',
      },
      advanced: {
        stage: 'Secondary',
        why: 'Keep environment controls available without making them part of the normal flow.',
        previous: 'Most people should stay in Documents and Ask.',
        next: 'Return to Documents or Ask after changing context.',
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
    documents: {
      title: 'Documents',
      summary: 'Upload documents, monitor progress, and keep the next step obvious.',
    },
    ask: {
      title: 'Ask',
      summary: 'Ask questions, review answers, and inspect related context.',
    },
    advanced: {
      title: 'Advanced',
      summary: 'Change workspace, library, and integrations only when needed.',
    },
  },
} as const
