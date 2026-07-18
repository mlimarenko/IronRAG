import type {
  DashboardAttentionItem as GeneratedDashboardAttentionItem,
  DashboardSurface,
  DocumentSummary,
  GraphStatus as GeneratedGraphStatus,
  GraphSurface,
  MessageLevel as GeneratedMessageLevel,
  WebIngestRunState as GeneratedWebIngestRunState,
  WebIngestRunSummary,
} from '@/shared/api/generated'

export type MessageLevel = GeneratedMessageLevel
export type GraphStatus = GeneratedGraphStatus
export type WebIngestRunState = GeneratedWebIngestRunState
export type DashboardAttentionItem = GeneratedDashboardAttentionItem
export type RecentDocument = DocumentSummary
export type DashboardGraph = GraphSurface
export type RecentWebRun = WebIngestRunSummary
export type DashboardData = DashboardSurface

export function buildDocumentsPath(filters: { documentId?: string; status?: string } = {}): string {
  const params = new URLSearchParams()
  if (filters.documentId) {
    params.set('documentId', filters.documentId)
  }
  if (filters.status) {
    params.set('status', filters.status)
  }
  const query = params.toString()
  return query ? `/documents?${query}` : '/documents'
}
