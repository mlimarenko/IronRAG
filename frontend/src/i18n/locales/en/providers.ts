export default {
  eyebrow: 'Admin flow',
  title: 'Providers',
  description:
    'Set up provider accounts and model profile defaults for each workspace without claiming the backend already supports full assignment workflows.',
  providerAccounts: 'Provider accounts',
  modelProfiles: 'Model profiles',
  providerKinds: {
    openai: 'OpenAI',
    deepseek: 'DeepSeek',
    compatible: 'Compatible API',
  },
  states: {
    loading: 'Loading',
    blocked: 'Blocked',
    attention: 'Needs attention',
    ready: 'Ready',
    idle: 'Idle',
    configured: 'Configured',
    missing: 'Missing',
    incomplete: 'Incomplete',
    available: 'Available',
    seeded: 'Seeded',
    pending: 'Pending',
    suggested: 'Suggested',
    todo: 'Needs setup',
  },
  actions: {
    refresh: 'Refresh governance',
  },
  loading: {
    title: 'Loading provider governance',
  },
  errors: {
    governanceUnavailableTitle: 'Provider governance unavailable',
    unknown: 'Unknown provider error',
  },
  empty: {
    noWorkspaceTitle: 'No workspace selected',
    noWorkspaceMessage: 'A workspace is required before provider setup guidance can render.',
    noWorkspaceHint:
      'Once workspace CRUD is wired end to end, this page can keep the same layout and swap in real create/edit flows.',
  },
  workspaceStrip: {
    eyebrow: 'Workspace context',
    description: 'Provider setup guidance for {slug}.',
  },
  wizard: {
    eyebrow: 'Provider setup',
    title: 'Provider setup wizard',
    description:
      'This guided flow helps operators sequence provider setup while staying clear about what the platform can already save and what still needs backend support.',
    providerKindsTitle: 'Pick a provider family',
    providerKindsDescription:
      'Use this to decide which provider account should land first for the selected workspace.',
    profileKindsTitle: 'Pick a profile family',
    profileKindsDescription:
      'Profile families mirror the current backend kinds and stay intentionally narrow.',
    nextActionTitle: 'What to do next',
    nextActionProvider: 'Create the first {provider} account for this workspace.',
    nextActionProfile: 'Add a {profile} model profile once the provider account exists.',
    nextActionAssignments:
      'Review recommended profile defaults. Project-level assignment flows still need backend support.',
    honestyNote:
      'Current backend capability: list and create provider accounts, list and create model profiles, and show governance summary.',
    steps: {
      providerAccount: {
        title: 'Provider account',
        description: 'Start with an API account record tied to the workspace.',
      },
      modelProfile: {
        title: 'Model profile',
        description: 'Attach a chat, embedding, or rerank profile to a provider account.',
      },
      assignments: {
        title: 'Recommended defaults',
        description: 'Preview likely pairings before real assignment UX arrives.',
      },
    },
    providerKinds: {
      openai: {
        label: 'OpenAI first',
        helper: 'Good default if you need both chat and embeddings quickly.',
      },
      deepseek: {
        label: 'DeepSeek first',
        helper: 'Useful for cost-aware chat experimentation where operators already use DeepSeek.',
      },
      compatible: {
        label: 'Compatible endpoint',
        helper: 'Reserve for self-hosted or OpenAI-compatible gateways.',
      },
    },
  },
  accounts: {
    eyebrow: 'Account inventory',
    description: 'Workspace-visible provider accounts currently returned by the backend.',
    emptyTitle: 'No provider accounts yet',
    emptyMessage: 'No {provider} account exists for this workspace yet.',
    emptyHint:
      'Secret entry, base URL validation, and credential testing still need dedicated backend/API support.',
  },
  profiles: {
    eyebrow: 'Profile inventory',
    description:
      'Model profiles are grouped by backend-supported kinds so later pickers can build on the same structure.',
    emptyTitle: 'No model profiles yet',
    emptyMessage: 'Create at least one model profile after a provider account is in place.',
    emptyHint:
      'Temperature, token limits, and capability metadata exist at creation time, but the current UI stays read-focused.',
    groupEmptyTitle: 'No {profile} profiles yet',
    groupEmptyMessage: 'This workspace does not have a {profile} profile configured yet.',
    kinds: {
      chat: {
        label: 'Chat',
        helper: 'Primary generation profile for answer synthesis.',
      },
      embedding: {
        label: 'Embedding',
        helper: 'Vectorization profile for indexing and retrieval.',
      },
      rerank: {
        label: 'Rerank',
        helper: 'Optional scoring profile for tighter retrieval ordering.',
      },
    },
  },
  recommendations: {
    eyebrow: 'Default profile recommendations',
    title: 'Recommended default profiles',
    description:
      'These cards establish the future picker layout: one recommended profile per capability, scoped to the selected workspace.',
    missingTitle: 'No {profile} recommendation yet',
    missingMessage: 'Create a {profile} model profile before this slot can suggest a default.',
  },
  summary: {
    providerAccounts: 'Provider accounts',
    modelProfiles: 'Model profiles',
    recommendedPairing: 'Suggested base account',
    providerAccountsHintReady: 'At least one provider account is available.',
    providerAccountsHintEmpty: 'Setup still starts at account creation.',
    modelProfilesHintReady: 'Profiles exist and can seed future pickers.',
    modelProfilesHintEmpty: 'No profile defaults can be inferred yet.',
    recommendedPairingInferred: 'Inferred from current inventory',
    recommendedPairingMissing: 'No account to suggest',
    recommendedPairingHint: 'The first matching provider account becomes the current suggestion.',
  },
} as const
