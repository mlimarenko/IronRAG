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

import type { DocumentStatus } from './documents'

export interface DashboardRecentDocument {
  id: string
  fileName: string
  fileType: string
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
  metrics: DashboardMetric[]
  attentionItems: DashboardAttentionItem[]
  recentDocuments: DashboardRecentDocument[]
  chartSummary: DashboardChartSummary | null
  primaryActions: DashboardPrimaryAction[]
}

export interface DashboardPrimaryAction {
  key: string
  label: string
  route: string
  icon: string | null
}
