import { describe, expect, it } from 'vitest'

import type { DashboardSurface } from '@/shared/api'

import {
  dashboardAdapter,
  dashboardFactMapPresentation,
  dashboardReadinessPresentation,
  dashboardWebRunTone,
} from './dashboardAdapter'

function dashboardSurface(overrides: Partial<DashboardSurface> = {}): DashboardSurface {
  return {
    documentMetrics: {
      total: 6,
      ready: 3,
      processing: 1,
      queued: 1,
      failed: 1,
      canceled: 0,
      empty: 0,
      graphReady: 2,
      graphSparse: 1,
      recomputedAt: '2026-08-09T09:18:00.000Z',
    },
    graph: {
      edgeCount: 63,
      edges: [],
      graphReadyDocumentCount: 2,
      graphSparseDocumentCount: 1,
      libraryId: 'library-1',
      nodeCount: 24,
      nodes: [],
      relationCount: 4,
      status: 'partial',
      typedFactDocumentCount: 2,
    },
    attention: [
      {
        code: 'failed_documents',
        detail: 'One document can be retried.',
        level: 'error',
        routePath: '/documents?status=failed',
        title: 'A document failed',
      },
    ],
    recentDocuments: [
      {
        canRetry: true,
        failureMessage: 'Embedding failed.',
        fileName: 'Runbook.pdf',
        fileSize: 2_048,
        fileType: 'pdf',
        id: 'document-1',
        readiness: 'failed',
        status: 'failed',
        uploadedAt: '2026-08-09T09:00:00.000Z',
      },
    ],
    recentWebRuns: [
      {
        boundaryPolicy: 'same_origin',
        counts: {
          blocked: 0,
          canceled: 0,
          discovered: 12,
          duplicates: 0,
          eligible: 12,
          excluded: 0,
          failed: 0,
          processed: 8,
          processing: 1,
          queued: 3,
        },
        crawlFilter: { allowPatterns: [], blockPatterns: [] },
        lastActivityAt: '2026-08-09T09:17:42.000Z',
        libraryId: 'library-1',
        materializationFilter: { allowPatterns: [], blockPatterns: [] },
        maxDepth: 2,
        maxPages: 100,
        mode: 'recursive_crawl',
        runId: 'run-1',
        runState: 'processing',
        seedUrl: 'https://docs.example.test',
      },
    ],
    warnings: [
      {
        code: 'vector_index_gap',
        detail: 'Some ready documents are missing their vectors.',
        level: 'warning',
        title: 'Ready documents are missing their vectors',
      },
    ],
    ...overrides,
  }
}

describe('dashboardAdapter', () => {
  it('preserves unpublished metadata absence and does not turn a real empty file into absence', () => {
    const base = dashboardSurface().recentDocuments[0]!
    const model = dashboardAdapter(
      dashboardSurface({
        recentDocuments: [
          { ...base, fileName: null, fileType: null, fileSize: null },
          { ...base, id: 'zero-byte', fileSize: 0 },
        ],
      }),
    )
    expect(model.recentDocuments[0]?.fileName).toBeNull()
    expect(model.recentDocuments[0]?.fileSize).toBeNull()
    expect(model.recentDocuments[1]?.fileSize).toBe(0)
  })
  it('bridges backend field names to the dashboard view model', () => {
    const model = dashboardAdapter(dashboardSurface())

    expect(model.metrics).toEqual({
      total: 6,
      ready: 3,
      processing: 1,
      queued: 1,
      failed: 1,
      canceled: 0,
      empty: 0,
      graphReady: 2,
      graphSparse: 1,
      recomputedAt: '2026-08-09T09:18:00.000Z',
    })
    expect(model.graph).toEqual({
      status: 'partial',
      nodes: 24,
      edges: 63,
      typedFactDocumentCount: 2,
    })
    expect(model.attention[0]).toMatchObject({
      routePath: '/documents?status=failed',
      severity: 'error',
    })
    expect(model.recentDocuments[0]).toMatchObject({
      id: 'document-1',
      progressPercent: null,
      canRetry: true,
    })
  })

  it('uses the first recent web run and handles an empty run list', () => {
    const populated = dashboardAdapter(dashboardSurface())
    const empty = dashboardAdapter(dashboardSurface({ recentWebRuns: [] }))

    expect(populated.lastWebRun).toEqual({
      state: 'processing',
      processed: 8,
      queued: 3,
      lastActivityAt: '2026-08-09T09:17:42.000Z',
    })
    expect(empty.lastWebRun).toBeNull()
  })

  it.each([
    ['graph_ready', { labelKey: 'dashboard.readyForSearch', tone: 'ready' }],
    ['readable', { labelKey: 'dashboard.readyForSearch', tone: 'ready' }],
    ['graph_sparse', { labelKey: 'dashboard.readyForSearch', tone: 'ready' }],
    ['processing', { labelKey: 'dashboard.preparingForSearch', tone: 'processing' }],
    ['failed', { labelKey: 'dashboard.reviewNeeded', tone: 'failed' }],
  ] as const)('maps %s readiness to a matching label and tone', (readiness, expected) => {
    expect(dashboardReadinessPresentation(readiness)).toEqual(expected)
  })

  it.each([
    [0, 0, 0, 0, 0, 'empty', { labelKey: 'dashboard.factMapNotAvailable', tone: 'neutral' }],
    [4, 4, 0, 0, 0, 'ready', { labelKey: 'dashboard.factMapComplete', tone: 'ready' }],
    [4, 2, 2, 0, 0, 'ready', { labelKey: 'dashboard.factMapIncomplete', tone: 'neutral' }],
    // Full coverage with a graph verdict other than `ready`: the relationships
    // exist, they just no longer match the current revisions. Reporting that as
    // missing coverage contradicted the 100% ring next to the badge.
    [4, 4, 0, 0, 0, 'stale', { labelKey: 'dashboard.factMapBehind', tone: 'neutral' }],
    // Full coverage while the graph itself is being rebuilt: the attempt is
    // graph-level, so both document counters are zero and only the status says
    // anything is running.
    [4, 4, 0, 0, 0, 'rebuilding', { labelKey: 'dashboard.factMapBuilding', tone: 'processing' }],
    [4, 2, 2, 0, 0, 'building', { labelKey: 'dashboard.factMapBuilding', tone: 'processing' }],
    // Ingest work does not establish that any relationship construction runs.
    [4, 2, 2, 1, 0, 'partial', { labelKey: 'dashboard.factMapIncomplete', tone: 'neutral' }],
    [4, 2, 2, 0, 3, 'partial', { labelKey: 'dashboard.factMapIncomplete', tone: 'neutral' }],
    [4, 4, 0, 1, 3, 'partial', { labelKey: 'dashboard.factMapComplete', tone: 'ready' }],
    // Same coverage gap with an idle queue: nothing is running, so nothing may
    // claim to be.
    [4, 2, 2, 0, 0, 'partial', { labelKey: 'dashboard.factMapIncomplete', tone: 'neutral' }],
  ] as const)(
    'maps ready=%s complete=%s limited=%s processing=%s queued=%s status=%s to a truthful fact-map badge',
    (ready, graphReady, graphSparse, processing, queued, status, expected) => {
      const metrics = { ready, graphReady, graphSparse, processing, queued }
      expect(dashboardFactMapPresentation(metrics, status)).toEqual(expected)
    },
  )

  it.each([
    ['accepted', 'processing'],
    ['discovering', 'processing'],
    ['processing', 'processing'],
    ['completed', 'ready'],
    ['completed_partial', 'warning'],
    ['failed', 'failed'],
    ['canceled', 'canceled'],
  ] as const)('maps %s web-run state to the %s badge tone', (state, tone) => {
    expect(dashboardWebRunTone(state)).toBe(tone)
  })
})
