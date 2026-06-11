import type { TFunction } from 'i18next';

import { buildDocumentFailureNotice, humanizeDocumentStage } from '@/shared/lib/document-processing';
import { mapSourceAccess } from '@/shared/lib/source-access';
import type { DocumentListItem } from '@/shared/api/documents';
import type { DocumentItem, DocumentStatus } from '@/shared/types';

function safeDecode(value: string): string {
  try {
    return decodeURIComponent(value);
  } catch {
    return value;
  }
}

function isHttpUrl(value: string | null | undefined): boolean {
  if (!value) {
    return false;
  }

  try {
    const parsed = new URL(value);
    return parsed.protocol === 'http:' || parsed.protocol === 'https:';
  } catch {
    return false;
  }
}

function extensionFromPathLike(value: string): string | null {
  const normalized = value.trim();
  if (!normalized) {
    return null;
  }

  const basename = normalized.split(/[\\/]/).at(-1) ?? normalized;
  const dotIndex = basename.lastIndexOf('.');
  if (dotIndex <= 0 || dotIndex >= basename.length - 1) {
    return null;
  }

  return basename.slice(dotIndex + 1).toLowerCase();
}

function deriveExtension(fileName: string, mimeType?: string | null): string {
  const normalizedFileName = isHttpUrl(fileName)
    ? safeDecode(new URL(fileName).pathname)
    : fileName;
  const fileNameExtension = extensionFromPathLike(normalizedFileName);
  if (fileNameExtension) {
    return fileNameExtension;
  }

  // Strip any `;charset=…` suffix the backend adds to the MIME tag.
  const baseMime = (mimeType ?? '').split(';')[0].trim().toLowerCase();
  if (!/^[a-z0-9.+-]+\/[a-z0-9.+-]+$/.test(baseMime)) {
    return 'file';
  }

  const slashIndex = baseMime.indexOf('/');
  if (slashIndex >= 0 && slashIndex < baseMime.length - 1) {
    return baseMime.slice(slashIndex + 1);
  }

  return 'file';
}

function normalizeProgressPercent(value: number | null | undefined): number | undefined {
  if (typeof value !== 'number' || !Number.isFinite(value)) {
    return undefined;
  }

  return Math.max(0, Math.min(100, Math.round(value)));
}

/**
 * Canonical list-row mapper. The backend list endpoint already emits
 * `DocumentListItem` with server-derived `status` / `readiness` / `stage`,
 * so the mapper only does view-model cosmetics: extension extraction,
 * URL-encoded name decoding, and `source_access` normalization.
 */
export function mapListItem(raw: DocumentListItem, t: TFunction): DocumentItem {
  const fileName = safeDecode(raw.fileName);
  const fileType = deriveExtension(fileName, raw.fileType);

  // Per-row cost arrives on every list response (see
  // list_document_page_rows LATERAL on billing_execution_cost). A valid
  // numeric value — including `0` — is preserved; only a missing /
  // non-numeric value collapses to `null`. The render path renders
  // `$0.000` for `0` and `—` for `null`, matching the prior UI.
  const costValue = parseFloat(raw.cost);
  const cost = Number.isFinite(costValue) ? costValue : null;
  const progressPercent = normalizeProgressPercent(raw.progressPercent);
  const failureCode = raw.failureCode ?? undefined;
  const failureMessage = raw.failureMessage?.trim() || undefined;
  const failureNotice =
    raw.status === 'failed'
      ? buildDocumentFailureNotice(
          {
            failureCode,
            failureMessage,
            stage: raw.stage,
          },
          t,
        )
      : undefined;
  const statusReason = failureNotice?.summary;

  return {
    id: raw.id,
    fileName,
    fileType,
    fileSize: raw.fileSize ?? 0,
    uploadedAt: raw.uploadedAt,
    cost,
    status: raw.status,
    readiness: raw.readiness,
    stage: humanizeDocumentStage(raw.stage, t),
    progressPercent,
    processingStartedAt: raw.processingStartedAt,
    processingFinishedAt: raw.processingFinishedAt,
    failureCode,
    failureMessage,
    failureNotice,
    statusReason,
    canRetry: raw.retryable,
    documentHint: raw.documentHint?.trim() || undefined,
    sourceKind: raw.sourceKind,
    sourceUri: raw.sourceUri,
    sourceAccess: mapSourceAccess(raw.sourceAccess),
  };
}

function parseTimestampMs(value: string | undefined): number | null {
  if (!value) {
    return null;
  }

  const timestamp = Date.parse(value);
  return Number.isFinite(timestamp) ? timestamp : null;
}

/**
 * Processing duration is the wall-clock time spent with the worker actively
 * holding the job (from `processingStartedAt`). Documents that never left
 * the queue have no start timestamp and therefore no timer — queued time is
 * not processing time. While the worker is still holding the job
 * (`processing` / `queued`) the timer keeps ticking against `nowMs`.
 */
export function getDocumentProcessingDurationMs(
  doc: Pick<DocumentItem, 'status' | 'processingStartedAt' | 'processingFinishedAt'>,
  nowMs = Date.now(),
): number | null {
  const startedAtMs = parseTimestampMs(doc.processingStartedAt);
  if (startedAtMs == null) {
    return null;
  }

  const workerStillHolding = doc.status === 'processing' || doc.status === 'queued';
  const finishedAtMs = workerStillHolding
    ? nowMs
    : parseTimestampMs(doc.processingFinishedAt);
  if (finishedAtMs == null) {
    return null;
  }

  return Math.max(0, finishedAtMs - startedAtMs);
}

type DocumentStatusBadge = { label: string; cls: string };

/**
 * Canonical badge styling and label lookup for every `DocumentStatus`.
 * Used by both the documents list and the inspector panel so every
 * surface agrees on how a given status is presented.
 */
export function buildDocumentStatusBadgeConfig(
  t: TFunction,
): Record<DocumentStatus, DocumentStatusBadge> {
  return {
    ready: { label: t('dashboard.statusLabels.ready'), cls: 'status-ready' },
    processing: {
      label: t('dashboard.statusLabels.processing'),
      cls: 'status-processing',
    },
    queued: { label: t('dashboard.statusLabels.queued'), cls: 'status-queued' },
    failed: { label: t('dashboard.statusLabels.failed'), cls: 'status-failed' },
    canceled: {
      label: t('dashboard.statusLabels.canceled'),
      cls: 'status-stalled',
    },
  };
}

export function isWebPageDocument(
  sourceKind: DocumentItem['sourceKind'],
  sourceUri?: string,
  fileName?: string,
): boolean {
  return sourceKind === 'web_page' || isHttpUrl(sourceUri) || isHttpUrl(fileName);
}

export function formatDocumentTypeLabel(
  fileType: string,
  sourceKind: DocumentItem['sourceKind'],
  t: TFunction,
  options?: {
    sourceUri?: string | undefined;
    fileName?: string | undefined;
  },
): string {
  if (isWebPageDocument(sourceKind, options?.sourceUri, options?.fileName)) {
    return t('documents.webPageType');
  }

  return fileType.toUpperCase();
}

export function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

export function formatDate(iso: string, locale: string): string {
  return new Intl.DateTimeFormat(locale, {
    month: 'short',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
  }).format(new Date(iso));
}
