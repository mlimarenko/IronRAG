import { defineStore } from 'pinia'
import { i18n } from 'src/lib/i18n'
import type {
  DashboardAttentionItem,
  DashboardChartSummary,
  DashboardMetric,
  DashboardOverviewSurface,
  DashboardPrimaryAction,
} from 'src/models/ui/dashboard'
import type { DocumentsSurfaceResponse } from 'src/models/ui/documents'
import type { GraphDiagnostics, GraphStatus } from 'src/models/ui/graph'
import { fetchDocumentsSurface, mapDashboardRecentDocuments } from 'src/services/api/documents'
import { fetchGraphDiagnostics, mapGraphDiagnosticsForDashboard } from 'src/services/api/graph'

interface DashboardState {
  activeLibraryId: string | null
  overview: DashboardOverviewSurface | null
  loading: boolean
  error: string | null
  loadRequestId: number
  refreshIntervalMs: number
}

const ACTIVE_REFRESH_INTERVAL_MS = 4_000

function isGraphActive(status: GraphStatus | null | undefined): boolean {
  return status === 'building' || status === 'rebuilding' || status === 'partial'
}

function buildPrimaryActions(): DashboardPrimaryAction[] {
  return [
    {
      key: 'documents',
      label: i18n.global.t('dashboard.actions.openDocuments'),
      route: '/documents',
      icon: null,
    },
    {
      key: 'graph',
      label: i18n.global.t('dashboard.actions.openGraph'),
      route: '/graph',
      icon: null,
    },
  ]
}

function buildStatusChartSummary(
  counters: DocumentsSurfaceResponse['counters'],
): DashboardChartSummary {
  const ready = counters.ready + counters.readyNoGraph
  return {
    label: i18n.global.t('dashboard.chart.title'),
    segments: [
      {
        key: 'ready',
        label: i18n.global.t('dashboard.chart.ready'),
        value: ready,
        color: 'var(--rr-success-text)',
      },
      {
        key: 'processing',
        label: i18n.global.t('dashboard.chart.processing'),
        value: counters.processing + counters.queued,
        color: '#eab308',
      },
      {
        key: 'failed',
        label: i18n.global.t('dashboard.chart.failed'),
        value: counters.failed,
        color: '#ef4444',
      },
    ],
  }
}

function buildOverviewSurface(
  documentsSurface: DocumentsSurfaceResponse,
  graphDiagnostics: GraphDiagnostics,
): DashboardOverviewSurface {
  const { t } = i18n.global
  const totalDocuments = documentsSurface.rows.length
  const inFlightCount = documentsSurface.counters.processing + documentsSurface.counters.queued
  const failedCount = documentsSurface.counters.failed
  const readyCount = documentsSurface.counters.ready + documentsSurface.counters.readyNoGraph
  const degradedWarnings = (documentsSurface.diagnostics.warnings ?? []).filter(
    (warning) => warning.isDegraded,
  )
  const graphSummary = mapGraphDiagnosticsForDashboard(graphDiagnostics)
  const attentionCount = failedCount + degradedWarnings.length + (graphSummary.attentionItem ? 1 : 0)

  const metrics: DashboardMetric[] = [
    {
      key: 'documents',
      label: t('dashboard.metrics.documents'),
      value: totalDocuments,
      trend: null,
      supportingText: t('dashboard.metricsHints.documents'),
    },
    {
      key: 'inFlight',
      label: t('dashboard.metrics.inFlight'),
      value: inFlightCount,
      trend: null,
      supportingText:
        inFlightCount > 0
          ? t('dashboard.metricsHints.inFlightActive')
          : t('dashboard.metricsHints.inFlightIdle'),
    },
    {
      key: 'ready',
      label: t('dashboard.metrics.ready'),
      value: readyCount,
      trend: null,
      supportingText:
        readyCount > 0
          ? t('dashboard.metricsHints.readyActive', { count: readyCount })
          : t('dashboard.metricsHints.readyIdle'),
    },
    {
      key: 'attention',
      label: t('dashboard.metrics.attention'),
      value: attentionCount,
      trend: null,
      supportingText:
        attentionCount > 0
          ? t('dashboard.metricsHints.attentionActive', { count: attentionCount })
          : t('dashboard.metricsHints.attentionQuiet'),
    },
  ]

  let summaryNarrative = t('dashboard.narrative.empty')
  if (totalDocuments > 0 && attentionCount > 0) {
    summaryNarrative = t('dashboard.narrative.attention', {
      failed: failedCount,
      inFlight: inFlightCount,
      graph: graphSummary.statusLabel,
    })
  } else if (totalDocuments > 0 && inFlightCount > 0) {
    summaryNarrative = t('dashboard.narrative.active', {
      total: totalDocuments,
      inFlight: inFlightCount,
      graph: graphSummary.statusLabel,
    })
  } else if (totalDocuments > 0) {
    summaryNarrative = t('dashboard.narrative.settled', {
      total: totalDocuments,
      ready: readyCount,
      graph: graphSummary.statusLabel,
    })
  }

  const attentionItems: DashboardAttentionItem[] = []
  if (failedCount > 0) {
    attentionItems.push({
      id: 'failed-documents',
      severity: 'error',
      title: t('dashboard.attentionItems.failedTitle'),
      message: t('dashboard.attentionItems.failedMessage', { count: failedCount }),
      targetRoute: '/documents',
      actionLabel: t('dashboard.attentionItems.failedAction'),
    })
  }
  if (degradedWarnings.length > 0) {
    attentionItems.push({
      id: 'document-warnings',
      severity: 'warning',
      title: t('dashboard.attentionItems.warningsTitle'),
      message: t('dashboard.attentionItems.warningsMessage', { count: degradedWarnings.length }),
      targetRoute: '/documents',
      actionLabel: t('dashboard.attentionItems.warningsAction'),
    })
  }
  if (graphSummary.attentionItem) {
    attentionItems.push(graphSummary.attentionItem)
  }

  return {
    summaryNarrative,
    metrics,
    attentionItems,
    recentDocuments: mapDashboardRecentDocuments(documentsSurface.rows),
    chartSummary: buildStatusChartSummary(documentsSurface.counters),
    primaryActions: buildPrimaryActions(),
  }
}

export const useDashboardStore = defineStore('dashboard', {
  state: (): DashboardState => ({
    activeLibraryId: null,
    overview: null,
    loading: false,
    error: null,
    loadRequestId: 0,
    refreshIntervalMs: 0,
  }),
  actions: {
    clear(): void {
      this.activeLibraryId = null
      this.overview = null
      this.loading = false
      this.error = null
      this.loadRequestId = 0
      this.refreshIntervalMs = 0
    },
    async load(libraryId: string | null, options?: { preserveUi?: boolean }): Promise<void> {
      if (!libraryId) {
        this.clear()
        return
      }

      const shouldShowLoading = !options?.preserveUi || !this.overview || this.activeLibraryId !== libraryId

      this.activeLibraryId = libraryId

      if (shouldShowLoading) {
        this.loading = true
      }

      this.error = null
      const requestId = ++this.loadRequestId

      try {
        const [documentsSurface, graphDiagnostics] = await Promise.all([
          fetchDocumentsSurface(),
          fetchGraphDiagnostics(libraryId),
        ])
        if (this.loadRequestId !== requestId || this.activeLibraryId !== libraryId) {
          return
        }
        this.overview = buildOverviewSurface(documentsSurface, graphDiagnostics)
        const hasDocumentWork =
          documentsSurface.counters.queued > 0 || documentsSurface.counters.processing > 0
        this.refreshIntervalMs =
          hasDocumentWork || isGraphActive(graphDiagnostics.graphStatus)
            ? ACTIVE_REFRESH_INTERVAL_MS
            : 0
      } catch (error) {
        if (this.loadRequestId !== requestId || this.activeLibraryId !== libraryId) {
          return
        }
        this.error = error instanceof Error ? error.message : 'Failed to load dashboard'
        throw error
      } finally {
        if (this.loadRequestId === requestId) {
          this.loading = false
        }
      }
    },
  },
})
