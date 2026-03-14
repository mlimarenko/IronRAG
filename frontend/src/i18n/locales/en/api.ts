export default {
  page: {
    eyebrow: 'Integrations',
    title: 'API & Integrations',
    description:
      'Live integration workspace for token inventory, backend endpoints, runnable examples, and project-scoped guidance built on current RustRAG surfaces.',
    loadingTitle: 'Loading API and integration context',
    actions: {
      refresh: 'Refresh API surface',
    },
    states: {
      loading: 'Loading',
      blocked: 'Blocked',
      foundation: 'Needs setup',
      ready: 'Ready',
    },
    errors: {
      title: 'API integration surface unavailable',
      unknown: 'Unknown API integration error',
    },
    empty: {
      noWorkspaceTitle: 'No workspace selected',
      noWorkspaceMessage:
        'Choose a workspace before rendering token inventory and integration guidance.',
      noWorkspaceHint:
        'This page is workspace-first because tokens, governance, and project access already scope through workspace IDs.',
    },
  },
  workspace: {
    eyebrow: 'Workspace context',
    description: 'Live integration guidance for {slug}.',
  },
  inventory: {
    eyebrow: 'Integration inventory',
    title: 'Current backend-backed inventory',
    description:
      'This panel stays honest about what exists now: token summaries, workspace governance counts, projects, and addressable endpoints.',
    ready: 'Surface loaded',
    needsTokens: 'Needs seed data',
    cards: {
      tokens: 'API tokens',
      tokensHint: 'Workspace-visible tokens from /v1/auth/tokens.',
      projects: 'Projects',
      projectsHint: 'Project scoping available for document and query calls.',
      providerAccounts: 'Provider accounts',
      providerAccountsHint: 'Workspace governance count used for integration readiness.',
      modelProfiles: 'Model profiles',
      modelProfilesHint: 'Available model profile count for query and ingest flows.',
    },
  },
  foundation: {
    workspaceContext: 'Workspace selected for scoped examples',
    tokenInventory: 'Token inventory loaded from auth surface',
    projectScope: 'At least one project available for integration examples',
    providerReadiness: 'Provider and model profile setup is present',
    ready: 'Ready',
    todo: 'Needs setup',
  },
  tokens: {
    eyebrow: 'Token inventory',
    title: 'API token inventory',
    description:
      'Read-only token summaries from the backend. Plaintext token values are intentionally not re-exposed after creation, so this page focuses on labels, scope shape, and operational recency.',
    available: 'Inventory visible',
    emptyBadge: 'No tokens',
    createdAt: 'Created',
    lastUsedAt: 'Last used',
    never: 'Never',
    emptyTitle: 'No API tokens yet',
    emptyMessage:
      'Mint a workspace or instance token before expecting integration examples to work outside the UI.',
    emptyHint:
      'The backend already supports POST /v1/auth/tokens. This page intentionally stops short of embedding secret minting UX.',
    scopeInventoryTitle: 'Scope inventory',
    scopeInventoryDescription: 'Aggregated scopes across currently visible tokens.',
  },
  examples: {
    eyebrow: 'Examples panel',
    title: 'Copy-pasteable backend examples',
    description:
      'Examples are tied to the selected workspace and optional project so future SDK snippets can reuse the same structure.',
    liveSurface: 'Current backend routes',
    shared: {
      tokenNote: 'Swap in a real bearer token, for example {token}.',
      workspaceScopeNote:
        'Workspace-scoped tokens can only operate inside their own workspace; project IDs must belong to that workspace.',
    },
    cards: {
      workspaceGovernance: {
        title: 'Read workspace governance',
        description: 'Fetch provider, profile, token, and usage counts for the selected workspace.',
        note: 'Useful for admin dashboards and install-time health checks.',
      },
      projectDocuments: {
        title: 'List project documents',
        description: 'Inspect content already attached to {project}.',
        note: 'Documents stay project-scoped even when the token is only workspace-scoped.',
      },
      runQuery: {
        title: 'Run a grounded query',
        description: 'Call the retrieval+answer path against the selected project.',
        note: 'Model profile selection is still optional in the request body if backend defaults are enough.',
      },
    },
  },
  guidance: {
    eyebrow: 'Project-scoped guidance',
    title: 'Integration guidance',
    description:
      'This section establishes the structure for per-project setup docs without inventing backend capabilities that do not exist yet.',
    projectScoped: 'Project scoped',
    workspaceScoped: 'Workspace scoped',
    allProjects: 'Workspace-wide view',
    workspaceTitle: 'Workspace integration guidance for {workspace}',
    workspaceDescription:
      'Use this mode when you are wiring admin or multi-project tooling and only know the workspace up front.',
    projectTitle: 'Integration guidance for {project}',
    projectDescription:
      'Use {project} when you need content, retrieval, or document APIs scoped to one project inside {workspace}.',
    bullets: {
      scopeWorkspace:
        'Keep the workspace slug ({workspace}) as the human-friendly anchor in docs and operator tooling.',
      scopeProject:
        'Persist the project slug ({project}) next to the project ID so integrations stay debuggable.',
      tokenReuse:
        'Prefer reusing a clearly labeled workspace token such as “{token}” instead of anonymous local secrets.',
      tokenMissing:
        'No workspace token is currently visible, so external integrations still need a token minting step.',
      permissionsScoped:
        'A non-instance token is already present, which is the right foundation for project-scoped automation instead of global admin sprawl.',
      permissionsAdminFallback:
        'If you only have instance-admin tokens today, treat them as bootstrap credentials and plan to downscope later.',
    },
  },
  endpoints: {
    eyebrow: 'Integration endpoints',
    title: 'Known backend endpoints',
    description:
      'This view lists the base URL plus the most relevant routes for the selected workspace and project context.',
    configured: 'Endpoint configured',
    unconfigured: 'Endpoint missing',
  },
} as const
