import type {
  DocumentPreparationReadinessKind,
  DocumentStatus,
  LibraryGraphCoverageSummary,
  LibraryReadinessSummary,
} from './documents'

export interface DashboardMetric {
  key: string
  label: string
  value: string | number
  trend: 'up' | 'down' | 'flat' | null
  supportingText: string | null
}

export interface DashboardAttentionItem {
  id: string
  severity: 'info' | 'warning' | 'error'
  title: string
  message: string
  targetRoute: string | null
  actionLabel: string | null
}

export interface DashboardHeroFact {
  key: string
  label: string
  value: string
  supportingText: string | null
  tone: 'default' | 'accent' | 'success' | 'warning'
}

export interface DashboardRecentDocument {
  id: string
  fileName: string
  fileType: string
  fileSizeLabel: string
  status: DocumentStatus
  statusLabel: string
  uploadedAt: string
}

export interface DashboardChartSummary {
  label: string
  segments: DashboardChartSegment[]
}

export interface DashboardChartSegment {
  key: string
  label: string
  value: number
  color: string | null
}

export interface DashboardOverviewSurface {
  summaryNarrative: string
  documentCounts: DashboardDocumentCounts
  readinessSummary: LibraryReadinessSummary | null
  graphCoverage: LibraryGraphCoverageSummary | null
  metrics: DashboardMetric[]
  attentionItems: DashboardAttentionItem[]
  recentDocuments: DashboardRecentDocument[]
  chartSummary: DashboardChartSummary | null
  primaryActions: DashboardPrimaryAction[]
}

export interface DashboardDocumentCounts {
  totalDocuments: number
  inFlightDocuments: number
  failedDocuments: number
  readableDocuments: number
  graphSparseDocuments: number
  graphReadyDocuments: number
}

export interface DashboardReadinessAttention {
  kind: DocumentPreparationReadinessKind
  count: number
}

export interface DashboardPrimaryAction {
  key: string
  label: string
  route: string
  icon: string | null
}

export function resolveDashboardVisibleMetrics(metrics: DashboardMetric[]): DashboardMetric[] {
  const metricMap = new Map(metrics.map((metric) => [metric.key, metric]))
  const documentCount = Number(metricMap.get('documents')?.value ?? 0)
  const readyCount = Number(
    metricMap.get('graphReady')?.value ?? metricMap.get('ready')?.value ?? 0,
  )
  const inFlightCount = Number(metricMap.get('inFlight')?.value ?? 0)
  const attentionCount = Number(metricMap.get('attention')?.value ?? 0)
  const fullySettled =
    documentCount > 0 && readyCount >= documentCount && inFlightCount === 0 && attentionCount === 0
  const sourceMetrics = metrics.filter((metric) => Number(metric.value) > 0)

  return (sourceMetrics.length > 0 ? sourceMetrics : metrics)
    .slice(0, 3)
    .filter(
      (metric) =>
        !(
          (metric.key === 'inFlight' && inFlightCount === 0) ||
          (metric.key === 'attention' && attentionCount === 0) ||
          ((metric.key === 'ready' || metric.key === 'graphReady') && fullySettled) ||
          (metric.key === 'documents' && documentCount === 0)
        ),
    )
}
