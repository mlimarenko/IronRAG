import type {
  DashboardAttentionItem as GeneratedDashboardAttentionItem,
  DashboardSurface,
  DocumentSummary,
  GraphStatus as GeneratedGraphStatus,
  GraphSurface,
  MessageLevel as GeneratedMessageLevel,
  WebIngestRunState as GeneratedWebIngestRunState,
  WebIngestRunSummary,
} from '@/shared/api/generated';

export type DashboardState = 'no-library' | 'loading' | 'loaded' | 'error';
export type MessageLevel = GeneratedMessageLevel;
export type GraphStatus = GeneratedGraphStatus;
export type WebIngestRunState = GeneratedWebIngestRunState;
export type DashboardAttentionItem = GeneratedDashboardAttentionItem;
export type RecentDocument = DocumentSummary;
export type DashboardGraph = GraphSurface;
export type RecentWebRun = WebIngestRunSummary;
export type DashboardData = DashboardSurface;

/**
 * DocumentsPage drives its list state from the server's keyset pagination and
 * only supports search + sort + an optional `documentId` deep-link. Dashboard
 * CTAs therefore resolve to either "open a specific document" or "open the
 * documents list" — status/readiness filters no longer exist as URL state.
 */
export function buildDocumentsPath(filters: { documentId?: string } = {}): string {
  if (filters.documentId) {
    const params = new URLSearchParams({ documentId: filters.documentId });
    return `/documents?${params.toString()}`;
  }
  return '/documents';
}
