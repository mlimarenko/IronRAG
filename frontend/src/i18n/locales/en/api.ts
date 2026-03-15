export default {
  page: {
    eyebrow: 'API launchpad',
    title: 'API Start',
    description:
      'Clear entry point for the current RustRAG API surface: public routes, bearer-token setup, scoped examples, and the groundwork already in place for future docs.',
    loadingTitle: 'Loading API launchpad',
    actions: {
      setup: 'Open setup',
      refresh: 'Refresh API context',
    },
    states: {
      loading: 'Loading',
      blocked: 'Blocked',
      foundation: 'Needs setup',
      ready: 'Ready',
    },
    errors: {
      title: 'API launchpad unavailable',
      unknown: 'Unknown API launchpad error',
    },
    empty: {
      noWorkspaceTitle: 'No workspace selected',
      noWorkspaceMessage:
        'Create or choose a workspace before rendering scoped API guidance.',
      noWorkspaceHint:
        'The API launchpad is workspace-first because projects, governance, and token visibility already pivot around workspace IDs.',
    },
  },
  workspace: {
    eyebrow: 'Workspace context',
    description: 'Scoped API guidance for {slug}.',
  },
  launchpad: {
    cards: {
      endpoint: 'Backend URL',
      endpointHint: 'Base endpoint used by the current frontend session.',
      auth: 'Session auth',
      authConnected: 'Bearer token saved',
      authMissing: 'No bearer token',
      authHintReady: 'Stored in browser session as {token}.',
      authHintMissing: 'Paste a token below to unlock auth-required examples and inventory.',
      workspace: 'Workspace scope',
      workspaceMissing: 'No workspace',
      workspaceHintReady: 'Current examples are scoped to {workspace}.',
      workspaceHintMissing: 'Select a workspace to render scoped routes.',
      project: 'Project scope',
      projectWorkspaceWide: 'Workspace-wide mode',
      projectHintReady: 'Query and document examples target {project}.',
      projectHintMissing: 'Keep workspace-wide mode or pick one project for deeper examples.',
    },
  },
  start: {
    eyebrow: 'Start here',
    title: 'Realistic first steps',
    description:
      'This page stays honest about what exists now: public discovery routes work immediately, auth-required routes need a real bearer token, and token minting still happens outside this UI.',
    status: {
      ready: 'Ready',
      saved: 'Saved',
      needsAction: 'Needs action',
      scoped: 'Scoped',
      needsSetup: 'Needs setup',
      live: 'Live',
      waiting: 'Waiting',
    },
    actions: {
      setup: 'Setup workspace',
      ingest: 'Prepare content',
    },
    steps: {
      endpoint: {
        title: 'Confirm the backend URL',
        description: 'Use this base URL for your first curl calls and future docs links.',
      },
      token: {
        title: 'Bring a real bearer token',
        description:
          'The UI can reuse an existing token in browser session storage, but it does not fake or mint one for you.',
        hintReady: 'Session token {token} is ready for protected reads.',
        hintMissing: 'Bring a token from your operator/bootstrap path before trying protected routes.',
      },
      scope: {
        title: 'Pick workspace and project scope',
        description:
          'Examples become more useful once they carry a real workspace ID and optional project ID.',
        descriptionReady: 'Current examples are already scoped to {workspace}.',
        hintProject: 'Project-specific examples currently target {project}.',
        hintWorkspace: 'Workspace-wide mode stays useful for discovery and admin tooling.',
      },
      requests: {
        title: 'Run public first, then protected calls',
        description:
          'Start with health and project discovery, then move to governance or query once auth is in place.',
        hintWithToken: 'Protected examples below are now ready to paste into a shell.',
        hintWithoutToken: 'Public examples below still work without any auth token.',
      },
    },
  },
  session: {
    eyebrow: 'Session auth',
    title: 'Session bearer token',
    description:
      'Store a token only for this browser session so the launchpad can probe protected surfaces without pretending a full auth UX already exists.',
    label: 'Bearer token',
    placeholder: 'rtrg_xxx_replace_me',
    activeLabel: 'Active session token',
    activeNone: 'No token saved',
    activeDescription: 'Protected API reads can use the saved bearer token until this browser session ends.',
    missingDescription: 'Public routes still work. Protected routes stay intentionally gated until a token is provided.',
    actions: {
      save: 'Save token',
      clear: 'Clear token',
    },
    status: {
      needsToken: 'Needs token',
      connected: 'Connected',
      limited: 'Limited access',
      verifying: 'Checking access',
      needsCheck: 'Needs check',
    },
    readiness: {
      governance: 'Workspace governance summary',
      tokens: 'Workspace token inventory',
      ready: 'Ready',
      needsToken: 'Needs token',
      unauthorized: 'Scope missing',
      error: 'Check failed',
    },
    notes: {
      sessionOnly: 'Token storage here is session-scoped and should be treated as a local testing convenience.',
      mintingOutsideUi:
        'Token creation, bootstrap flows, and secret distribution are still external to this page.',
      plaintextOnce:
        'Plaintext tokens are returned once at mint time and should not be expected to reappear here.',
    },
  },
  inventory: {
    eyebrow: 'Current surface',
    title: 'Backend-backed inventory',
    description:
      'Public and auth-required surfaces are separated here so the page remains useful before auth is wired end-to-end.',
    ready: 'Protected surface visible',
    needsToken: 'Public-only mode',
    cards: {
      workspaces: 'Workspaces',
      workspacesHint: 'Public list from /v1/workspaces.',
      projects: 'Projects',
      projectsHint: 'Project scoping available without auth for discovery.',
      tokens: 'API tokens',
      tokensHint: 'Token summaries visible from /v1/auth/tokens when scope allows it.',
      tokensPendingHint: 'Requires a bearer token with token inventory access.',
      providerAccounts: 'Provider accounts',
      providerAccountsHint: 'Count sourced from workspace governance.',
      modelProfiles: 'Model profiles',
      modelProfilesHint: 'Count sourced from workspace governance.',
      protectedPendingHint: 'Visible after protected workspace reads succeed.',
    },
  },
  foundation: {
    workspaceContext: 'Workspace selected for scoped API examples',
    sessionToken: 'Session token saved for protected API reads',
    projectScope: 'At least one project is available for scoped requests',
    protectedReadiness: 'At least one protected surface can be reached',
    ready: 'Ready',
    todo: 'Needs setup',
  },
  tokens: {
    eyebrow: 'Token inventory',
    title: 'Workspace token inventory',
    description:
      'Token summaries stay read-only here. This page shows labels, scopes, and recency, but intentionally does not pretend to mint or re-expose secrets.',
    available: 'Inventory visible',
    emptyBadge: 'No tokens',
    createdAt: 'Created',
    lastUsedAt: 'Last used',
    never: 'Never',
    missingTokenTitle: 'Add a session token first',
    missingTokenMessage:
      'Token inventory is protected, so this launchpad cannot list tokens until you provide a real bearer token.',
    missingTokenHint:
      'Use the session auth panel above after minting a token elsewhere in your operator/bootstrap flow.',
    unauthorizedTitle: 'Token inventory scope missing',
    unauthorizedMessage:
      'The saved bearer token reached the backend, but it does not have enough scope to list workspace tokens.',
    unauthorizedHint:
      'Use a token with workspace-admin capabilities if you want this page to render token inventory.',
    errorTitle: 'Token inventory check failed',
    emptyTitle: 'No API tokens yet',
    emptyMessage:
      'The protected inventory route is reachable, but no visible tokens were returned for this workspace.',
    emptyHint:
      'The backend can mint tokens, but this page intentionally stops short of a full secret-management flow.',
    scopeInventoryTitle: 'Scope inventory',
    scopeInventoryDescription: 'Aggregated scopes across currently visible tokens.',
  },
  examples: {
    eyebrow: 'First requests',
    title: 'Copy-pasteable first calls',
    description:
      'These examples are intentionally split between public discovery routes and protected routes that require a real bearer token.',
    liveSurface: 'Current runtime surface',
    access: {
      public: 'Public route',
      token: 'Token required',
    },
    shared: {
      tokenExport: 'Export a real token into `RUSTRAG_TOKEN` before running protected calls.',
      workspaceScopeNote:
        'Keep workspace and project IDs aligned so scoped calls stay debuggable and predictable.',
    },
    cards: {
      health: {
        title: 'Check service health',
        description: 'Confirm the backend is up before attempting any scoped integration flow.',
        note: 'Useful for smoke tests, deploy checks, and docs quickstarts.',
      },
      projects: {
        title: 'List workspace projects',
        description: 'Discover projects for the selected workspace before choosing a query target.',
        note: 'This is a clean first call when you only know the workspace ID.',
      },
      workspaceGovernance: {
        title: 'Read workspace governance',
        description: 'Fetch provider, profile, token, and usage counts for the selected workspace.',
        note: 'Good next step once bearer auth is in place.',
      },
      runQuery: {
        title: 'Run a grounded query',
        description: 'Exercise the main retrieval-and-answer path against the selected project.',
        note: 'Treat this as the first realistic product-facing API example after discovery.',
      },
    },
  },
  guidance: {
    eyebrow: 'Scoped guidance',
    title: 'Workspace and project guidance',
    description:
      'This section keeps future API docs honest by anchoring examples in the actual workspace and project context available right now.',
    projectScoped: 'Project scoped',
    workspaceScoped: 'Workspace scoped',
    allProjects: 'Workspace-wide view',
    workspaceTitle: 'Workspace integration guidance for {workspace}',
    workspaceDescription:
      'Use this mode when you are wiring admin tooling or cross-project automation and only know the workspace up front.',
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
        'No visible workspace token is available here yet, so external integrations still need a token minting step elsewhere.',
      permissionsScoped:
        'A non-instance token is already present, which is the right foundation for project-scoped automation instead of global admin sprawl.',
      permissionsAdminFallback:
        'If you only have instance-admin tokens today, treat them as bootstrap credentials and downscope later.',
    },
  },
  groundwork: {
    eyebrow: 'Docs groundwork',
    title: 'What is already in place',
    description:
      'The product UI now has enough structure to grow into full API docs, typed examples, and an eventual auth walkthrough without inventing capabilities that are not ready.',
    status: {
      ready: 'Ready',
      next: 'Next layer',
    },
    cards: {
      contract: {
        title: 'OpenAPI source of truth',
        description: 'A hand-maintained contract already exists for the runtime API surface.',
        note: 'Keep the contract aligned with backend routes before publishing broader docs.',
      },
      types: {
        title: 'Generated frontend types',
        description: 'Frontend contract types are already generated from the OpenAPI document.',
      },
      examples: {
        title: 'Scoped example structure',
        description: 'This page now renders examples from live workspace and project context instead of static placeholder docs.',
        note: 'That keeps future snippets grounded in real IDs, scopes, and backend URLs.',
      },
      next: {
        title: 'Missing docs layer',
        description: 'Published docs, SDK examples, and a full bootstrap auth walkthrough are still not productized.',
        note: 'Those pieces should be added on top of the contract pipeline and this launchpad, not faked inside it.',
      },
    },
  },
  endpoints: {
    eyebrow: 'Known endpoints',
    title: 'Configured API endpoints',
    description:
      'This view lists the current base URL plus the most relevant routes for the selected workspace and project context.',
    configured: 'Endpoint configured',
    unconfigured: 'Endpoint missing',
  },
} as const
