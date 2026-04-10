import type { TFunction } from 'i18next';

import { humanizeDocumentFailure, humanizeDocumentStage } from '@/lib/document-processing';
import { mapSourceAccess } from '@/lib/source-access';
import type { DocumentItem, DocumentReadiness, DocumentStatus } from '@/types';

/**
 * The document list/detail endpoint currently emits a mix of camelCase and
 * snake_case fields. This captures exactly the nested fields the documents
 * UI reads (the raw backend payload is intentionally richer).
 */
interface RawDocumentRevision {
  mime_type?: string;
  byte_size?: number;
  content_source_kind?: string;
  source_uri?: string;
  revision_number?: number;
}

export interface RawDocumentForUI {
  id?: string;
  fileName?: string;
  activeRevision?: RawDocumentRevision;
  active_revision?: RawDocumentRevision;
  document?: {
    id?: string;
    external_key?: string;
    created_at?: string;
  };
  readinessSummary?: {
    readinessKind?: string;
    activityStatus?: string;
    graphCoverageKind?: string;
    stalledReason?: string;
  };
  readiness_summary?: {
    readiness_kind?: string;
    activity_status?: string;
    stalled_reason?: string;
  };
  pipeline?: {
    latest_job?: {
      queue_state?: string;
      current_stage?: string;
      failure_code?: string;
      retryable?: boolean;
    };
  };
  sourceAccess?: unknown;
}

export type DocumentsStatusFilter = 'all' | 'in_progress' | 'ready' | 'failed';

export const PAGE_SIZE_OPTIONS = [50, 100, 250, 1000] as const;

export function parseStatusFilter(value: string | null): DocumentsStatusFilter {
  if (value === 'in_progress' || value === 'ready' || value === 'failed') {
    return value;
  }

  return 'all';
}

export function parseReadinessFilter(value: string | null): DocumentReadiness | null {
  if (
    value === 'processing' ||
    value === 'readable' ||
    value === 'graph_sparse' ||
    value === 'graph_ready' ||
    value === 'failed'
  ) {
    return value;
  }

  return null;
}

export function parsePageSize(value: string | null): (typeof PAGE_SIZE_OPTIONS)[number] {
  const parsed = Number.parseInt(value ?? '', 10);

  if (PAGE_SIZE_OPTIONS.includes(parsed as (typeof PAGE_SIZE_OPTIONS)[number])) {
    return parsed as (typeof PAGE_SIZE_OPTIONS)[number];
  }

  return PAGE_SIZE_OPTIONS[0];
}

export function parsePage(value: string | null): number {
  const parsed = Number.parseInt(value ?? '', 10);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : 1;
}

export function mapApiDocument(raw: RawDocumentForUI, t: TFunction): DocumentItem {
  const fileName = raw.fileName
    ?? raw.activeRevision?.title ?? raw.active_revision?.title
    ?? raw.document?.external_key ?? 'unknown';
  const extension = fileName.includes('.') ? fileName.split('.').pop()?.toLowerCase() ?? '' : '';
  const mimeType = raw.activeRevision?.mime_type ?? raw.active_revision?.mime_type ?? '';
  const fileType = extension || mimeType.split('/').pop() || 'file';
  const fileSize = raw.activeRevision?.byte_size ?? raw.active_revision?.byte_size ?? 0;
  const uploadedAt = raw.document?.created_at ?? '';

  const readinessKind = raw.readinessSummary?.readinessKind ?? raw.readiness_summary?.readiness_kind ?? '';
  const jobState = raw.pipeline?.latest_job?.queue_state ?? '';
  const jobStage = raw.pipeline?.latest_job?.current_stage ?? undefined;
  const failureCode = raw.pipeline?.latest_job?.failure_code ?? undefined;
  const retryable = raw.pipeline?.latest_job?.retryable ?? false;
  const activityStatus = raw.readinessSummary?.activityStatus ?? raw.readiness_summary?.activity_status ?? '';

  let readiness: DocumentReadiness = 'processing';
  if (readinessKind === 'graph_ready') readiness = 'graph_ready';
  else if (readinessKind === 'graph_sparse') readiness = 'graph_sparse';
  else if (readinessKind === 'readable') readiness = 'readable';
  else if (readinessKind === 'failed' || jobState === 'failed') readiness = 'failed';

  let status: DocumentStatus = 'processing';
  if (readiness === 'graph_ready' || readiness === 'readable') status = 'ready';
  else if (readiness === 'graph_sparse') status = 'ready_no_graph';
  else if (readiness === 'failed') status = 'failed';
  else if (jobState === 'queued' || activityStatus === 'queued') status = 'queued';

  const failureMessage =
    readiness === 'failed'
      ? humanizeDocumentFailure(
          {
            failureCode,
            stalledReason:
              raw.readinessSummary?.stalledReason ?? raw.readiness_summary?.stalled_reason,
            stage: jobStage,
          },
          t,
        )
      : undefined;

  const revision = raw.activeRevision ?? raw.active_revision;

  return {
    id: raw.document?.id ?? raw.id ?? '',
    fileName,
    fileType,
    fileSize,
    uploadedAt,
    cost: null,
    status,
    readiness,
    stage: humanizeDocumentStage(jobStage, t),
    failureMessage,
    canRetry: readiness === 'failed' ? retryable : undefined,
    sourceKind: revision?.content_source_kind ?? undefined,
    sourceUri: revision?.source_uri ?? undefined,
    sourceAccess: mapSourceAccess(raw.sourceAccess),
  };
}

export function formatSize(bytes: number) {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

export function formatDate(iso: string, locale: string) {
  return new Intl.DateTimeFormat(locale, {
    month: 'short',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
  }).format(new Date(iso));
}
