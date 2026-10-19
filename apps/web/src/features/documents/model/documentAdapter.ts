import type { TFunction } from 'i18next'

import { buildDocumentFailureNotice, humanizeDocumentStage } from '@/shared/lib/document-processing'
import { mapSourceAccess } from '@/shared/lib/source-access'
import type { DocumentListItem } from '@/shared/api/documents'
import type { DocumentItem } from '@/shared/types'
import { revisionSourceKindLabel } from '@/shared/lib/enumLabels'

function safeDecode(value: string): string {
  try {
    return decodeURIComponent(value)
  } catch {
    return value
  }
}

function isHttpUrl(value: string | null | undefined): boolean {
  if (!value) {
    return false
  }

  try {
    const parsed = new URL(value)
    return parsed.protocol === 'http:' || parsed.protocol === 'https:'
  } catch {
    return false
  }
}

function extensionFromPathLike(value: string): string | null {
  const normalized = value.trim()
  if (!normalized) {
    return null
  }

  const segments = normalized.split(/[\\/]/)
  const basename = segments.at(-1) ?? normalized
  const dotIndex = basename.lastIndexOf('.')
  if (dotIndex <= 0 || dotIndex >= basename.length - 1) {
    return null
  }

  return basename.slice(dotIndex + 1).toLowerCase()
}

function deriveExtension(fileName: string | null, mimeType?: string | null): string {
  const normalizedFileName =
    fileName && isHttpUrl(fileName) ? safeDecode(new URL(fileName).pathname) : fileName
  const fileNameExtension = normalizedFileName ? extensionFromPathLike(normalizedFileName) : null
  if (fileNameExtension) {
    return fileNameExtension
  }

  // Strip any `;charset=…` suffix the backend adds to the MIME tag.
  const baseMime = ((mimeType ?? '').split(';')[0] ?? '').trim().toLowerCase()
  if (!/^[a-z0-9.+-]+\/[a-z0-9.+-]+$/.test(baseMime)) {
    return 'file'
  }

  const slashIndex = baseMime.indexOf('/')
  if (slashIndex >= 0 && slashIndex < baseMime.length - 1) {
    return baseMime.slice(slashIndex + 1)
  }

  return 'file'
}

function normalizeProgressPercent(value: number | null | undefined): number | undefined {
  if (typeof value !== 'number' || !Number.isFinite(value)) {
    return undefined
  }

  return Math.max(0, Math.min(100, Math.round(value)))
}

function normalizeDocumentCost(value: DocumentListItem['cost']): string | null {
  // Per-row cost arrives from billing_execution_cost on every list response.
  // Preserve valid decimals exactly: Number would lose precision and disagree
  // with the server facet value and selection key.
  return value != null && /^-?\d+(?:\.\d+)?$/.test(value) ? value : null
}

/**
 * Shared list-row mapper. The backend list endpoint already emits
 * `DocumentListItem` with server-derived `status` / `readiness` / `stage`,
 * so the mapper only does view-model cosmetics: extension extraction,
 * URL-encoded name decoding, and `source_access` normalization.
 */
export function mapListItem(
  raw: DocumentListItem,
  t: TFunction,
  costRollupFreshness?: 'current' | 'reconciling',
): DocumentItem {
  const fileName = raw.fileName ?? null
  const fileType = raw.fileType ?? null

  const cost = normalizeDocumentCost(raw.cost)
  const progressPercent = normalizeProgressPercent(raw.progressPercent)
  const failureCode = raw.failureCode ?? undefined
  const failureMessage = raw.failureMessage?.trim() || undefined
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
      : undefined
  const statusReason = failureNotice?.summary
  const stage = humanizeDocumentStage(raw.stage, t)
  const documentHint = raw.documentHint?.trim() || undefined
  const sourceAccess = mapSourceAccess(raw.sourceAccess)

  // Every field below is optional on `DocumentItem` without an explicit
  // `| undefined`, so — under `exactOptionalPropertyTypes` — an
  // undefined-valued key must be omitted rather than assigned. The
  // conditional spreads below are behaviorally identical to a present
  // key holding `undefined` (both read back as `undefined`); they exist
  // purely to satisfy the exact-optional contract without widening
  // `DocumentItem` itself.
  return {
    id: raw.id,
    documentState: raw.documentState,
    fileName,
    fileType,
    fileSize: raw.fileSize ?? null,
    uploadedAt: raw.uploadedAt,
    cost,
    ...(costRollupFreshness ? { costRollupFreshness } : {}),
    status: raw.status,
    readiness: raw.readiness,
    externalKey: raw.externalKey,
    ...(stage !== undefined ? { stage } : {}),
    ...(progressPercent !== undefined ? { progressPercent } : {}),
    ...(raw.processingStartedAt != null ? { processingStartedAt: raw.processingStartedAt } : {}),
    ...(raw.processingFinishedAt != null ? { processingFinishedAt: raw.processingFinishedAt } : {}),
    ...(failureCode !== undefined ? { failureCode } : {}),
    ...(failureMessage !== undefined ? { failureMessage } : {}),
    ...(failureNotice !== undefined ? { failureNotice } : {}),
    ...(statusReason !== undefined ? { statusReason } : {}),
    canRetry: raw.retryable,
    ...(documentHint !== undefined ? { documentHint } : {}),
    ...(raw.sourceKind != null ? { sourceKind: raw.sourceKind } : {}),
    ...(raw.sourceUri != null ? { sourceUri: raw.sourceUri } : {}),
    ...(sourceAccess !== undefined ? { sourceAccess } : {}),
  }
}

function parseTimestampMs(value: string | undefined): number | null {
  if (!value) {
    return null
  }

  const timestamp = Date.parse(value)
  return Number.isFinite(timestamp) ? timestamp : null
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
  const startedAtMs = parseTimestampMs(doc.processingStartedAt)
  if (startedAtMs == null) {
    return null
  }

  const workerStillHolding = doc.status === 'processing' || doc.status === 'queued'
  const finishedAtMs = resolveFinishedAtMs(doc, nowMs, workerStillHolding)
  if (finishedAtMs == null || !Number.isFinite(finishedAtMs)) {
    return null
  }

  return Math.max(0, finishedAtMs - startedAtMs)
}

/**
 * The wall-clock instant a document's processing finished, or `null` when it
 * never has: the live clock while the worker still holds the job, the recorded
 * finish time once it exists, and nothing for a document that never started.
 */
function resolveFinishedAtMs(
  doc: Pick<DocumentItem, 'status' | 'processingFinishedAt'>,
  nowMs: number,
  workerStillHolding: boolean,
): number | null {
  if (doc.status === 'processing') return nowMs
  const finishedAtMs = parseTimestampMs(doc.processingFinishedAt)
  if (finishedAtMs != null) return finishedAtMs
  return workerStillHolding ? nowMs : null
}

/**
 * Whether the documents surface may offer a reprocess for this document.
 *
 * `canRetry` (the server's `retryable`) answers "may this *failed* attempt be
 * retried". A `ready` document has no failed attempt to retry, which is a
 * different question from "may its index be rebuilt": the server accepts a
 * reprocess for it, so `canRetry === false` must not disable the control. Work
 * in flight is never reprocessed blindly. Deleted documents and terminal
 * attempts marked non-retryable stay off; replacement source provenance does
 * not mean the current document is inactive.
 */
export function canReprocessDocument(
  document: Pick<DocumentItem, 'documentState' | 'status' | 'canRetry'>,
): boolean {
  if (document.documentState !== 'active') return false
  if (document.status === 'queued' || document.status === 'processing') return false
  if (document.status === 'ready') return true
  return document.canRetry === true
}

/** Editor format and icons are presentation decisions, not the table's MIME sort/filter key. */
export function getDocumentSourceFormat(
  document: Pick<DocumentItem, 'fileName' | 'fileType'>,
): string {
  return deriveExtension(
    document.fileName === null ? null : safeDecode(document.fileName),
    document.fileType,
  )
}

function isWebPageDocument(
  sourceKind: DocumentItem['sourceKind'],
  sourceUri?: string,
  fileName?: string | null,
): boolean {
  return sourceKind === 'web_page' || isHttpUrl(sourceUri) || isHttpUrl(fileName)
}

export function formatDocumentTypeLabel(
  fileType: string | null,
  sourceKind: DocumentItem['sourceKind'],
  t: TFunction,
  options?: {
    sourceUri?: string | undefined
    fileName?: string | null | undefined
  },
): string {
  if (
    fileType === null &&
    options?.fileName == null &&
    options?.sourceUri == null &&
    sourceKind == null
  ) {
    return t('common.notAvailable')
  }
  if (isWebPageDocument(sourceKind, options?.sourceUri, options?.fileName)) {
    return t('documents.webPageType')
  }

  return getDocumentSourceFormat({ fileName: options?.fileName ?? null, fileType }).toUpperCase()
}

export function formatDocumentSourceLabel(document: DocumentItem, t: TFunction): string {
  const externalKey = document.externalKey?.toLowerCase() ?? ''
  if (externalKey.startsWith('confluence:page:')) {
    return t('documents.sourceKinds.confluencePage')
  }
  if (isWebPageDocument(document.sourceKind, document.sourceUri, document.fileName)) {
    return t('documents.sourceKinds.webPage')
  }
  if (document.sourceKind) {
    return revisionSourceKindLabel(document.sourceKind, t)
  }
  return t('documents.sourceKinds.upload')
}
