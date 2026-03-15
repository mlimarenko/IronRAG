export default {
  page: {
    eyebrow: 'Knowledge graph',
    title: 'Graph',
    description:
      'Inspect graph readiness, search graph concepts, and review what entities or relations are already visible versus still waiting on backend support.',
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
} as const
