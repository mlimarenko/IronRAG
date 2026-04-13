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
  title?: string;
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
      queued_at?: string;
      claimed_at?: string;
      completed_at?: string;
    };
  };
  sourceAccess?: unknown;
}

export type DocumentsStatusFilter = 'all' | 'in_progress' | 'attention' | 'ready' | 'failed';

export const PAGE_SIZE_OPTIONS = [50, 100, 250, 1000] as const;

export function parseStatusFilter(value: string | null): DocumentsStatusFilter {
  if (
    value === 'in_progress' ||
    value === 'attention' ||
    value === 'ready' ||
    value === 'failed'
  ) {
    return value;
  }

  return 'all';
}

/**
 * Severity ranking used by the documents-list status column sort. Lower
 * numbers represent calmer states ("everything's fine, ready"); higher
 * numbers represent states that need operator attention ("worker is stuck").
 * Ascending sort puts the calm states at the top and pushes problems down;
 * descending sort flips it so a single click on the Status header surfaces
 * the documents that need looking at right now.
 *
 * The ordering is deliberate, not alphabetical:
 *   ready < ready_no_graph < processing < queued < canceled < retrying < failed < blocked < stalled
 */
export function documentStatusSortRank(status: DocumentStatus): number {
  switch (status) {
    case 'ready':
      return 0;
    case 'ready_no_graph':
      return 1;
    case 'processing':
      return 2;
    case 'queued':
      return 3;
    case 'canceled':
      return 4;
    case 'retrying':
      return 5;
    case 'failed':
      return 6;
    case 'blocked':
      return 7;
    case 'stalled':
      return 8;
  }
}

/**
 * Lumps every `DocumentStatus` into the coarse bucket used by the filter pills
 * and the aggregate counts. One place where the mapping lives so the filter
 * row, the status counts, and any future ops overview stay in sync.
 *
 * - `in_progress`: queued, processing, retrying — the worker is (or will soon
 *   be) moving the document forward.
 * - `attention`: stalled, blocked — the worker is holding the job but nothing
 *   is progressing; a human or an external dependency needs to unblock it.
 * - `ready`: ready (full graph) and ready_no_graph (graph sparse) — the
 *   document is usable by retrieval to some degree.
 * - `failed`: failed and canceled — terminal states nothing will retry from
 *   automatically. We group canceled with failed because from the "what
 *   should I look at" standpoint they are both "not going to finish".
 */
export function documentStatusBucket(
  status: DocumentStatus,
): 'in_progress' | 'attention' | 'ready' | 'failed' {
  switch (status) {
    case 'queued':
    case 'processing':
    case 'retrying':
      return 'in_progress';
    case 'stalled':
    case 'blocked':
      return 'attention';
    case 'ready':
    case 'ready_no_graph':
      return 'ready';
    case 'failed':
    case 'canceled':
      return 'failed';
  }
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

function parseTimestampMs(value: string | undefined): number | null {
  if (!value) {
    return null;
  }

  const timestamp = Date.parse(value);
  return Number.isFinite(timestamp) ? timestamp : null;
}

/**
 * A document status where the worker has claimed the job and has not yet
 * released it — active processing, stalled heartbeat, blocked on a dependency,
 * or mid-retry. In all of these the elapsed-time meter should keep ticking
 * from `claimed_at` so the UI shows how long the worker has been holding it.
 */
function workerIsHoldingJob(status: DocumentStatus): boolean {
  return (
    status === 'processing' ||
    status === 'stalled' ||
    status === 'blocked' ||
    status === 'retrying'
  );
}

/**
 * Processing duration is the wall-clock time spent with the worker actively
 * holding the job (from `claimed_at`). Documents still waiting in the queue
 * have no `processingStartedAt` and therefore no timer — queued time is not
 * processing time.
 */
export function getDocumentProcessingDurationMs(
  doc: Pick<DocumentItem, 'status' | 'processingStartedAt' | 'processingFinishedAt'>,
  nowMs = Date.now(),
): number | null {
  const startedAtMs = parseTimestampMs(doc.processingStartedAt);
  if (startedAtMs == null) {
    return null;
  }

  const finishedAtMs = workerIsHoldingJob(doc.status)
    ? nowMs
    : parseTimestampMs(doc.processingFinishedAt);
  if (finishedAtMs == null) {
    return null;
  }

  return Math.max(0, finishedAtMs - startedAtMs);
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
  const activityStatus =
    raw.readinessSummary?.activityStatus ?? raw.readiness_summary?.activity_status ?? '';
  const stalledReason =
    raw.readinessSummary?.stalledReason ?? raw.readiness_summary?.stalled_reason ?? undefined;
  const jobState = raw.pipeline?.latest_job?.queue_state ?? '';
  const jobStage = raw.pipeline?.latest_job?.current_stage ?? undefined;
  const failureCode = raw.pipeline?.latest_job?.failure_code ?? undefined;
  const retryable = raw.pipeline?.latest_job?.retryable ?? false;
  const claimedAt = raw.pipeline?.latest_job?.claimed_at ?? undefined;
  const completedAt = raw.pipeline?.latest_job?.completed_at ?? undefined;

  // Readiness reflects only how readable the document content is, independent
  // of where the job currently sits in the queue. Anything that has not yet
  // reached a terminal readiness level (readable / graph_sparse / graph_ready /
  // failed) stays at 'processing' as a placeholder.
  let readiness: DocumentReadiness = 'processing';
  if (readinessKind === 'graph_ready') readiness = 'graph_ready';
  else if (readinessKind === 'graph_sparse') readiness = 'graph_sparse';
  else if (readinessKind === 'readable') readiness = 'readable';
  else if (readinessKind === 'failed' || jobState === 'failed' || jobState === 'canceled') {
    readiness = 'failed';
  }

  // Status derivation is a strict priority chain with a single rule: terminal
  // queue states (`canceled`, `failed`, `completed`) dominate, then ready
  // content displays as ready regardless of any pending refinement work, then
  // a `completed` job that *failed* to produce ready content is surfaced as
  // a terminal failure (zombie), then in-flight (`leased`) jobs use the
  // backend's `activity_status` to differentiate live from stuck, and
  // finally everything else is plain `queued`. `activity_status` only ever
  // modifies the `leased` case — for queued or terminal states it is
  // meaningless (no claim, or claim already gone), so the mapper does not
  // let it override those states the way the previous version did.
  //
  // Concretely, this fixes two categories of misclassification:
  //   - 4800+ "zombie" documents with `queue_state='completed'` but
  //     `readiness='processing'` (ingest finished without ever reaching a
  //     readable state, e.g. because cancel landed mid-pipeline) used to
  //     show up as `stalled` under "Needs Attention". They are actually
  //     terminal failures — show them under Failed.
  //   - Documents in `queue_state='queued'` whose previous attempt left a
  //     stale `claimed_at` would inherit `activity_status='stalled'` from
  //     `derive_queued_status` and end up under "Needs Attention", even
  //     though they are simply waiting for a worker. They are now plain
  //     `queued` again.
  let status: DocumentStatus;
  if (jobState === 'canceled') {
    status = 'canceled';
  } else if (jobState === 'failed' || readiness === 'failed') {
    status = 'failed';
  } else if (readiness === 'graph_ready' || readiness === 'readable') {
    status = 'ready';
  } else if (readiness === 'graph_sparse') {
    status = 'ready_no_graph';
  } else if (jobState === 'completed') {
    // Job is terminal but readiness never reached a usable level. This is
    // a broken/incomplete ingest, not an in-flight state. Surface it as
    // `failed` so it leaves "Needs Attention" / "In Progress" and shows up
    // in the Failed bucket where the operator can retry or delete it.
    status = 'failed';
  } else if (jobState === 'leased') {
    // Worker holds the job. activity_status discriminates between an
    // actively-progressing pipeline and a stuck one.
    if (activityStatus === 'blocked') status = 'blocked';
    else if (activityStatus === 'retrying') status = 'retrying';
    else if (activityStatus === 'stalled') status = 'stalled';
    else status = 'processing';
  } else {
    // jobState is 'queued' or absent. Always plain 'queued' — a queued doc
    // has no live claim, so any `activity_status='stalled'` left over from
    // a previous attempt is irrelevant here.
    status = 'queued';
  }

  // Surface a human-readable reason for any state that needs explaining —
  // terminal failures, stalls, and blocks all want the why on the badge.
  // For the "zombie" case (`completed` job that did not produce ready
  // content), we synthesize a specific reason because the job has no
  // `failure_code` of its own — `humanizeDocumentFailure` would otherwise
  // fall back to the generic "unknown error" message.
  const isZombieCompletion =
    status === 'failed' &&
    jobState === 'completed' &&
    readiness !== 'failed' &&
    !failureCode;
  const diagnosticReason = isZombieCompletion
    ? t('documents.failureMessages.completedWithoutReadable')
    : status === 'failed' || status === 'stalled' || status === 'blocked'
      ? humanizeDocumentFailure({ failureCode, stalledReason, stage: jobStage }, t)
      : undefined;
  const failureMessage = status === 'failed' ? diagnosticReason : undefined;

  const revision = raw.activeRevision ?? raw.active_revision;

  // Processing starts only when the worker claims the job. A missing
  // `claimed_at` means the document is still queued and has no timer —
  // there is intentionally no fallback to `queued_at` or `uploadedAt`.
  const processingStartedAt = claimedAt;
  const processingFinishedAt = status === 'processing' ? undefined : completedAt;

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
    processingStartedAt,
    processingFinishedAt,
    failureMessage,
    statusReason: diagnosticReason,
    canRetry: readiness === 'failed' ? retryable : undefined,
    sourceKind: revision?.content_source_kind ?? undefined,
    sourceUri: revision?.source_uri ?? undefined,
    sourceAccess: mapSourceAccess(raw.sourceAccess),
  };
}

export type DocumentStatusBadge = { label: string; cls: string };

/**
 * Canonical badge styling and label lookup for every `DocumentStatus`. Used
 * by both the documents list and the inspector panel so every surface agrees
 * on how a given status is presented.
 *
 * Color assignment favors visual distinctness for the failure family — a
 * stalled worker must not look like a normal processing tick, otherwise the
 * "everything says processing but nothing is moving" bug comes back.
 */
export function buildDocumentStatusBadgeConfig(
  t: TFunction,
): Record<DocumentStatus, DocumentStatusBadge> {
  return {
    queued: { label: t('dashboard.statusLabels.queued'), cls: 'status-queued' },
    processing: { label: t('dashboard.statusLabels.processing'), cls: 'status-processing' },
    retrying: { label: t('dashboard.statusLabels.retrying'), cls: 'status-warning' },
    blocked: { label: t('dashboard.statusLabels.blocked'), cls: 'status-warning' },
    stalled: { label: t('dashboard.statusLabels.stalled'), cls: 'status-stalled' },
    canceled: { label: t('dashboard.statusLabels.canceled'), cls: 'status-queued' },
    ready: { label: t('dashboard.statusLabels.ready'), cls: 'status-ready' },
    ready_no_graph: { label: t('dashboard.statusLabels.ready_no_graph'), cls: 'status-warning' },
    failed: { label: t('dashboard.statusLabels.failed'), cls: 'status-failed' },
  };
}

export function formatDocumentTypeLabel(
  fileType: string,
  sourceKind: DocumentItem['sourceKind'],
  t: TFunction,
): string {
  if (sourceKind === 'web_page') {
    return t('documents.webPageType');
  }

  return fileType.toUpperCase();
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
