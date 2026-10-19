import type { DashboardSurface } from '@/shared/api'

type DashboardAttentionSeverity = 'info' | 'warning' | 'error'

export type DashboardReadinessPresentation = {
  labelKey:
    | 'dashboard.readyForSearch'
    | 'dashboard.preparingForSearch'
    | 'dashboard.emptyForSearch'
    | 'dashboard.reviewNeeded'
  tone: 'ready' | 'neutral' | 'sparse' | 'processing' | 'failed'
}

export type DashboardFactMapPresentation = {
  labelKey:
    | 'dashboard.factMapNotAvailable'
    | 'dashboard.factMapComplete'
    | 'dashboard.factMapIncomplete'
    | 'dashboard.factMapBehind'
    | 'dashboard.factMapBuilding'
  tone: 'ready' | 'neutral' | 'processing'
}

export type DashboardViewModel = {
  metrics: {
    total: number
    ready: number
    processing: number
    queued: number
    failed: number
    canceled: number
    empty: number
    graphReady: number
    graphSparse: number
    recomputedAt: string
  }
  graph: {
    status: DashboardSurface['graph']['status']
    nodes: number
    edges: number
    typedFactDocumentCount: number
  }
  attention: Array<{
    code: string
    routePath: string
    severity: DashboardAttentionSeverity
  }>
  recentDocuments: Array<{
    id: string
    fileName: string | null
    fileSize: number | null
    uploadedAt: string
    readiness: DashboardSurface['recentDocuments'][number]['readiness']
    progressPercent: number | null
    canRetry: boolean
  }>
  lastWebRun: {
    state: DashboardSurface['recentWebRuns'][number]['runState']
    processed: number
    queued: number
    lastActivityAt: string | null
  } | null
}

function mapSeverity(
  level: DashboardSurface['attention'][number]['level'],
): DashboardAttentionSeverity {
  switch (level) {
    case 'error':
      return 'error'
    case 'warning':
      return 'warning'
    case 'info':
      return 'info'
  }
}

export function dashboardReadinessPresentation(
  readiness: DashboardSurface['recentDocuments'][number]['readiness'],
): DashboardReadinessPresentation {
  switch (readiness) {
    case 'graph_ready':
      return { labelKey: 'dashboard.readyForSearch', tone: 'ready' }
    case 'readable':
      return { labelKey: 'dashboard.readyForSearch', tone: 'ready' }
    case 'graph_sparse':
      return { labelKey: 'dashboard.readyForSearch', tone: 'ready' }
    case 'processing':
      return { labelKey: 'dashboard.preparingForSearch', tone: 'processing' }
    case 'empty':
      return { labelKey: 'dashboard.emptyForSearch', tone: 'neutral' }
    case 'failed':
      return { labelKey: 'dashboard.reviewNeeded', tone: 'failed' }
  }
}

// Document ingest can be queued without any graph work being active.
// Only a graph-level running verdict can announce relationship construction.
export function dashboardFactMapPresentation(
  metrics: Pick<DashboardViewModel['metrics'], 'ready' | 'graphReady' | 'graphSparse'>,
  status: DashboardSurface['graph']['status'],
): DashboardFactMapPresentation {
  if (metrics.ready === 0) {
    return { labelKey: 'dashboard.factMapNotAvailable', tone: 'neutral' }
  }

  // `building` and `rebuilding` are graph-level verdicts: an attempt over the
  // whole projection is live even when no single document is leased or queued,
  // so the document counters can both be zero while the graph is genuinely
  // being rebuilt. Reading only the document lifecycle called that state
  // "coverage is incomplete" on a library whose ring showed 100%.
  const isGraphWorkRunning = status === 'building' || status === 'rebuilding'
  if (isGraphWorkRunning) {
    return { labelKey: 'dashboard.factMapBuilding', tone: 'processing' }
  }

  if (status === 'stale') {
    return { labelKey: 'dashboard.factMapBehind', tone: 'neutral' }
  }

  const isFullyCovered = metrics.graphSparse === 0 && metrics.graphReady === metrics.ready
  return isFullyCovered
    ? { labelKey: 'dashboard.factMapComplete', tone: 'ready' }
    : { labelKey: 'dashboard.factMapIncomplete', tone: 'neutral' }
}

export function dashboardWebRunTone(
  state: DashboardSurface['recentWebRuns'][number]['runState'],
): 'processing' | 'ready' | 'warning' | 'failed' | 'canceled' {
  switch (state) {
    case 'accepted':
    case 'discovering':
    case 'processing':
      return 'processing'
    case 'completed':
      return 'ready'
    case 'completed_partial':
      return 'warning'
    case 'failed':
      return 'failed'
    case 'canceled':
      return 'canceled'
  }
}

export function dashboardAdapter(surface: DashboardSurface): DashboardViewModel {
  const lastWebRun = surface.recentWebRuns[0]

  return {
    metrics: {
      total: surface.documentMetrics.total,
      ready: surface.documentMetrics.ready,
      processing: surface.documentMetrics.processing,
      queued: surface.documentMetrics.queued,
      failed: surface.documentMetrics.failed,
      canceled: surface.documentMetrics.canceled,
      empty: surface.documentMetrics.empty,
      graphReady: surface.documentMetrics.graphReady,
      graphSparse: surface.documentMetrics.graphSparse,
      recomputedAt: surface.documentMetrics.recomputedAt,
    },
    graph: {
      status: surface.graph.status,
      nodes: surface.graph.nodeCount,
      edges: surface.graph.edgeCount,
      typedFactDocumentCount: surface.graph.typedFactDocumentCount,
    },
    attention: surface.attention.map((item) => ({
      code: item.code,
      routePath: item.routePath,
      severity: mapSeverity(item.level),
    })),
    recentDocuments: surface.recentDocuments.map((document) => ({
      id: document.id,
      fileName: document.fileName ?? null,
      fileSize: document.fileSize ?? null,
      uploadedAt: document.uploadedAt,
      readiness: document.readiness,
      progressPercent: document.progressPercent ?? null,
      canRetry: document.canRetry,
    })),
    lastWebRun: lastWebRun
      ? {
          state: lastWebRun.runState,
          processed: lastWebRun.counts.processed,
          queued: lastWebRun.counts.queued,
          lastActivityAt: lastWebRun.lastActivityAt ?? null,
        }
      : null,
  }
}
