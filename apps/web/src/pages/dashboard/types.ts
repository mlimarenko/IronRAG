import type { DocumentReadiness } from '@/types';

export type DashboardState = 'no-library' | 'loading' | 'loaded' | 'error';
export type MessageLevel = 'info' | 'warning' | 'error';

export type GraphStatus =
  | 'empty'
  | 'building'
  | 'rebuilding'
  | 'ready'
  | 'partial'
  | 'failed'
  | 'stale';

export type WebIngestRunState =
  | 'accepted'
  | 'discovering'
  | 'processing'
  | 'completed'
  | 'completed_partial'
  | 'failed'
  | 'canceled';

export interface DashboardOverview {
  totalDocuments: number;
  readyDocuments: number;
  processingDocuments: number;
  failedDocuments: number;
  graphSparseDocuments: number;
}

export interface DashboardMetric {
  key: string;
  value: string;
  level: MessageLevel;
}

export interface DashboardAttentionItem {
  code: string;
  title: string;
  detail: string;
  routePath: string;
  level: MessageLevel;
}

export interface RecentDocument {
  id: string;
  fileName: string;
  fileSize: number;
  uploadedAt: string;
  readiness: DocumentReadiness;
  stageLabel?: string | null;
  failureMessage?: string | null;
  canRetry: boolean;
  preparedSegmentCount?: number | null;
  technicalFactCount?: number | null;
}

export interface DashboardGraph {
  status: GraphStatus;
  warning?: string | null;
  nodeCount: number;
  edgeCount: number;
  graphReadyDocumentCount: number;
  graphSparseDocumentCount: number;
  typedFactDocumentCount: number;
  updatedAt?: string | null;
}

export interface WebRunCounts {
  discovered: number;
  eligible: number;
  processed: number;
  queued: number;
  processing: number;
  blocked: number;
  failed: number;
}

export interface RecentWebRun {
  runId: string;
  runState: WebIngestRunState;
  seedUrl: string;
  counts: WebRunCounts;
  lastActivityAt?: string | null;
}

export interface DashboardData {
  overview: DashboardOverview;
  metrics: DashboardMetric[];
  recentDocuments: RecentDocument[];
  recentWebRuns: RecentWebRun[];
  graph: DashboardGraph;
  attention: DashboardAttentionItem[];
}

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
