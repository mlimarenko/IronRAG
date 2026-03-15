export default {
  page: {
    eyebrow: 'Knowledge graph',
    title: 'Graph',
    description:
      'Inspect graph readiness, search graph concepts, and review what entities or relations are already visible versus still waiting on backend support.',
    technicalNote:
      'This is a secondary technical surface for graph coverage and readiness checks. Stay in Search for the primary answer workflow.',
    statusLabel: 'Preview',
  },
  hero: {
    status: 'Preview',
    headline:
      'Use Graph to inspect entity coverage, relation visibility, and retrieval-linked evidence as graph data becomes available.',
    body: 'This workspace already gives operators a clear map of what graph evidence exists today, what comes from retrieval detail, and which graph records are still waiting on backend support.',
    highlights: [
      'Retrieval detail already captures references, matched chunks, and raw debug payloads.',
      'Search and detail panels stay explicit about which graph records are available right now.',
      'As dedicated graph APIs come online, this view can switch from guidance to live entity and relation inspection without changing the workflow.',
    ],
  },
  metrics: {
    entityCoverage: {
      label: 'Entity coverage',
      value: 'Awaiting the first retrieval run',
    },
    relationFreshness: {
      label: 'Relation freshness',
      value: 'No graph snapshots yet',
    },
    operatorPosture: {
      label: 'Operator posture',
      value: 'Workspace ready for graph signals',
    },
  },
  summary: {
    eyebrow: 'Current coverage',
    title: 'Graph summary',
    description:
      'What RustRAG can show today, what comes from retrieval, and where backend graph records are still missing.',
    status: 'Preview',
    rows: {
      sourceOfTruth: {
        label: 'Current source of truth',
        value: 'Retrieval run detail and graph-backed metadata',
      },
      availableEvidence: {
        label: 'Available graph evidence',
        value: 'References, matched chunks, debug payload',
      },
      unavailable: {
        label: 'Still unavailable',
        value: 'Entity list, relation edges, provenance-rich graph API',
      },
    },
  },
  search: {
    eyebrow: 'Discovery',
    title: 'Graph search',
    description:
      'Search across available graph signals and the backend capabilities this workspace is still waiting for.',
    label: 'Search graph concepts',
    placeholder: 'Search entities, relations, retrieval, debug...',
    empty: {
      title: 'No graph matches yet',
      message: 'No graph concepts on this page match that search yet.',
      hint: 'Try broader terms like retrieval, relation, entity, or debug. This becomes richer as graph APIs and indexed records arrive.',
    },
    results: {
      retrievalSignals: {
        title: 'Retrieval signals',
        kind: 'Available now',
        summary:
          'Current graph-adjacent data comes from retrieval references, matched chunks, and debug JSON captured per run.',
        evidence: ['references[]', 'matched_chunk_ids[]', 'debug_json'],
      },
      entityIndex: {
        title: 'Entity index',
        kind: 'Awaiting backend data',
        summary:
          'Entity search is ready to display results, but no backend endpoint exposes canonical entities yet.',
        evidence: ['Needs graph entity list API', 'Needs project-scoped indexing'],
      },
      relationInspector: {
        title: 'Relation inspector',
        kind: 'Awaiting backend data',
        summary:
          'Relation detail is prepared for operator review, but relation tuples are not returned by the platform today.',
        evidence: ['Needs relation edges API', 'Needs provenance payload'],
      },
    },
  },
  detail: {
    eyebrow: 'Detail',
    title: 'Graph detail',
    description:
      'Review the selected concept, what evidence is available now, and whether live relation records can already be inspected.',
    waitingOnApi: 'Waiting on API',
    evidenceTitle: 'Available evidence',
    relationTitle: 'Relation view',
    emptyRelations: {
      title: 'No live relation edges yet',
      message: 'The backend does not expose canonical relation tuples for this concept yet.',
      hint: 'As soon as graph APIs provide relation data, this panel should show provenance-rich edges and neighbors instead of explanatory text.',
    },
    emptySelection: {
      title: 'No graph detail selected',
      message: 'Pick a graph concept from search to review available evidence and current backend coverage.',
      hint: 'This keeps the page actionable without inventing entities or relations that do not exist yet.',
    },
    relations: {
      retrievalSignals: [
        {
          from: 'Retrieval run',
          relation: 'records',
          to: 'References',
        },
        {
          from: 'Retrieval run',
          relation: 'matches',
          to: 'Chunk IDs',
        },
        {
          from: 'Retrieval run',
          relation: 'captures',
          to: 'Debug payload',
        },
      ],
    },
  },
  states: {
    chooseProject: 'Choose project',
    loadingSurface: 'Loading graph surface',
    backendPending: 'Backend entry point pending',
    surfaceDegraded: 'Graph surface degraded',
  },
  actions: {
    processing: 'Setup scope',
    ingest: 'Ingest content',
  },
  surface: {
    noProject: {
      status: 'Blocked',
      headline: 'Select a project to inspect graph relations.',
      body: 'This screen is ready to show persisted entities and relation coverage as soon as a project scope is selected.',
      highlights: [
        'Project scope comes from the same workspace flow used by Files and Search.',
        'The page stays explicit about missing context instead of inventing graph data.',
        'Once a project is selected, the screen probes live graph endpoints immediately.',
      ],
    },
    unavailable: {
      status: 'Entry point ready',
      headline: 'Graph UI is wired, but this backend build does not expose graph runtime routes yet.',
      body: 'The product surface is project-scoped and ready for real graph data, but `/graph-products/*` still needs backend wiring in the running environment.',
      highlights: [
        'No fake entities or relations are rendered when the route is unavailable.',
        'Project selection, status mapping, and empty states are already product-ready.',
        'The same screen will light up automatically once graph routes ship on the backend.',
      ],
    },
    live: {
      status: 'Live graph rows',
      headline: 'Inspect persisted entities, relation coverage, and search results for the selected project.',
      body: 'This view is reading real graph rows. Relation search and entity detail are live where the backend has persisted records.',
      highlights: [
        'Search results come from persisted entities and relation rows, not placeholder text.',
        'Entity detail exposes aliases, supporting documents, chunk references, and observed relations.',
        'Warnings stay visible when extraction tracking or provenance depth are still partial.',
      ],
    },
    waiting: {
      status: 'Waiting for extraction',
      headline: 'Graph endpoints respond, but this project has no persisted relation rows yet.',
      body: 'The screen is live against the backend, and the current blocker is runtime extraction populating entity and relation rows for this project.',
      highlights: [
        'The page confirms backend reachability even when graph counts are zero.',
        'Entity and relation counts stay at zero until extraction writes persisted rows.',
        'As soon as rows appear, search and detail panels switch to live data without UI changes.',
      ],
    },
  },
  metricLabels: {
    entities: 'Entities',
    relations: 'Relations',
    extractionRuns: 'Extraction runs',
    noProjectSelected: 'No project selected',
    awaitingProjectScope: 'Awaiting project scope',
    backendRoutePending: 'Backend route pending',
  },
  panels: {
    summary: {
      eyebrow: 'Scope and readiness',
      title: 'Graph summary',
      description:
        'Project-scoped graph readiness, live coverage, and the blocker that still keeps relation extraction partial.',
      workspace: 'Workspace',
      workspaceEmpty: 'No workspace selected',
      project: 'Project',
      projectPlaceholder: 'Select a project',
      relationKinds: 'Relation kinds',
      entityKinds: 'Entity kinds',
      currentBlocker: 'Current blocker',
      blockerApiUnavailable: 'Backend route is not wired in this runtime build yet.',
      blockerPartial: 'Extraction tracking and provenance depth remain partial.',
      blockerNoRows: 'Runtime extraction has not written entity and relation rows for this project yet.',
    },
    search: {
      eyebrow: 'Discovery',
      title: 'Graph search',
      description:
        'Search persisted entities and relations when the graph runtime is available. Without a query, the panel shows top entities and sample relations.',
      label: 'Search graph concepts',
      placeholder: 'Search entities, relations, aliases...',
      loading: 'Loading graph',
      noProject: {
        title: 'Select a project first',
        message: 'Graph is scoped per project. Choose a project to inspect entity and relation coverage.',
        hint: 'The selector in this panel uses the same session scope as the rest of the operator shell.',
      },
      unavailable: {
        title: 'Graph backend route is not available',
        message: 'This product surface is ready, but the running backend does not expose `/graph-products/*` yet.',
        hint: 'Backend wiring is the remaining blocker before live entity and relation data can appear here.',
      },
      noMatches: {
        title: 'No graph matches yet',
        message: 'No persisted entities or relations matched that search.',
        hint: 'Try broader terms like a canonical entity name, alias, or relation type.',
      },
      noRows: {
        title: 'No graph rows yet',
        message: 'This project does not have persisted graph rows yet.',
        hint: 'Once extraction writes entity and relation rows, the search panel will populate automatically.',
      },
      searching: 'Searching graph records...',
    },
    detail: {
      eyebrow: 'Detail',
      title: 'Graph detail',
      description:
        'Inspect persisted entity evidence, bounded subgraph neighbors, and relation coverage without inventing provenance that the backend does not yet return.',
      loading: 'Loading detail',
      loadErrorTitle: 'Entity detail could not be loaded',
      loadErrorHint:
        'Coverage and search results can still be reviewed while backend detail for this entity is investigated.',
      emptySelection: {
        title: 'No graph detail selected',
        message: 'Pick an entity or relation from the search panel to inspect live graph coverage.',
        hint: 'The detail panel only renders persisted graph data and explicit blockers.',
      },
      technicalSummary: 'Show technical graph controls',
      technicalHint: 'Adjust subgraph depth and inspect bounded graph structure only when needed.',
      subgraphSummary: 'Show bounded subgraph and relation structure',
      subgraphDepth: 'Subgraph depth',
      entitySummary: '{count} observed relations connected to this entity.',
      aliases: 'Aliases',
      noAliases: 'No aliases were persisted for this entity.',
      documents: 'Source document ids',
      noDocuments: 'No source document ids were persisted for this entity yet.',
      chunks: 'Source chunk ids',
      noChunks: 'No source chunk ids were persisted for this entity yet.',
      subgraphEyebrow: 'Subgraph',
      subgraphTitle: '{name} neighborhood',
      subgraphStats: '{entities} entities · {relations} relations',
      subgraphHint: 'Bounded graph expansion is currently loaded at depth {depth}.',
      subgraphEntities: 'Entities in subgraph',
      noSubgraphEntities: 'No neighboring entities were returned for this bounded subgraph.',
      subgraphRelations: 'Relations in subgraph',
      noSubgraphRelations: 'No persisted relations were returned for this bounded subgraph yet.',
      outgoingRelations: 'Outgoing relations',
      noOutgoingRelations: 'This entity currently has no outgoing persisted relations.',
      incomingRelations: 'Incoming relations',
      noIncomingRelations: 'This entity currently has no incoming persisted relations.',
      matchReasons: 'Match reasons',
      noMatchReasons: 'This record came from the live summary rather than a query-specific search match.',
    },
    diagnostics: {
      eyebrow: 'Diagnostics',
      title: 'Graph diagnostics',
      description:
        'See live content counts, provenance coverage, readiness blockers, and the next operator-visible step for this project.',
      pending: 'Awaiting diagnostics',
      loading: 'Loading diagnostics',
      noProject: {
        title: 'Select a project first',
        message: 'Diagnostics are scoped per project. Choose one to inspect graph readiness and blockers.',
        hint: 'The same selected project drives graph search, detail, and diagnostics.',
      },
      unavailable: {
        title: 'Graph diagnostics route is not available',
        message: 'This runtime does not expose graph diagnostics yet.',
        hint: 'Once the backend route is live, this panel will show content counts, provenance gaps, and next steps.',
      },
      metrics: {
        documents: 'Persisted documents',
        chunks: 'Persisted chunks',
        embeddings: 'Embedded chunks',
        retrievalRuns: 'Retrieval runs',
        entityRefs: 'Entities with chunk refs',
        relationRefs: 'Relations with chunk refs',
      },
      blockersTitle: 'Current blockers',
      noBlockers: 'No explicit blockers were returned.',
      nextStepsTitle: 'Next steps',
      noNextSteps: 'No next steps were returned.',
      technicalSummary: 'Show technical coverage metrics',
      technicalHint: 'Document, chunk, embedding, and provenance counts stay available here as secondary diagnostics.',
    },
  },
  common: {
    noGraphRows: 'No graph rows yet',
  },
  errors: {
    loadEntityDetail: 'Failed to load entity detail',
    loadPageContext: 'Failed to load graph page context',
    loadCoverage: 'Failed to load graph coverage',
    searchFailed: 'Graph search failed',
  },
} as const
