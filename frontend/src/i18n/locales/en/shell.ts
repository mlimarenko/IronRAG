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
        hint: 'Extra context and integration controls when needed.',
      },
    },
  },
  topbar: {
    surface: 'Current page',
    language: 'Language',
    languageHint: 'Interface',
  },
  context: {
    eyebrow: 'Current context',
    workspace: 'Workspace',
    library: 'Library',
    none: 'Not selected',
    loading: 'Loading your workspace and library…',
    empty: 'A default workspace and library will appear here when available.',
    emptyWorkspaceHint:
      'No workspace is available yet. Open Advanced only if the default context did not appear.',
    emptyLibraryHint:
      'Your workspace is ready, but no library is available yet. Open Advanced only if you need to add one manually.',
    workspaceOnly: '{workspace} is ready. Pick a library when it appears.',
    ready: '{workspace} · {library}',
    defaultWorkspace: 'Default workspace',
    defaultLibrary: 'Default library',
    advanced: 'Advanced controls',
    advancedHint:
      'Only use these controls when the default context is missing or you need a different workspace or library.',
    advancedCreate: 'Create another workspace or library only when the default one is not enough.',
    advancedManage: 'Rename or remove items only from this secondary area.',
    manage: 'Open Advanced',
    backToDocuments: 'Back to Documents',
    error: 'Could not load workspace and library.',
    errorSummary: 'Workspace and library controls are unavailable right now.',
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
