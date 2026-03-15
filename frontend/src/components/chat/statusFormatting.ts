import { i18n } from 'src/boot/i18n'
import {
  formatDebugValue as formatDebugValueLabel,
  translateStatusLabel,
} from 'src/i18n/helpers'

export function getStatusTone(status?: string): 'positive' | 'warning' | 'negative' | 'neutral' {
  const normalized = status?.toLowerCase() ?? ''

  if (['grounded', 'complete', 'ok', 'success'].includes(normalized)) {
    return 'positive'
  }

  if (
    ['partial', 'weakly_grounded', 'weak', 'degraded', 'warning', 'fallback'].includes(
      normalized,
    )
  ) {
    return 'warning'
  }

  if (['failed', 'error', 'ungrounded', 'empty', 'blocked'].includes(normalized)) {
    return 'negative'
  }

  return 'neutral'
}

export function formatStatusLabel(status?: string): string {
  return translateStatusLabel(status)
}

export function formatDebugEntries(debugJson: Record<string, unknown>) {
  return Object.entries(debugJson).map(([key, value]) => ({
    key,
    preview: formatDebugValue(value),
  }))
}

function formatDebugValue(value: unknown): string {
  return formatDebugValueLabel(value)
}

export function formatReferenceTitle(reference: string, index: number): string {
  const parsed = parseReference(reference)

  if (parsed.chunkId) {
    return i18n.global.t('flow.search.diagnostics.referenceTitles.passage', { index: index + 1 }) as string
  }

  if (parsed.documentId) {
    return i18n.global.t('flow.search.diagnostics.referenceTitles.document', { index: index + 1 }) as string
  }

  return i18n.global.t('flow.search.diagnostics.referenceTitles.reference', {
    index: index + 1,
  }) as string
}

export function formatReferenceMeta(reference: string): string {
  const parsed = parseReference(reference)

  if (parsed.documentId && parsed.chunkId) {
    return i18n.global.t('flow.search.diagnostics.referenceMeta.documentChunk', {
      documentId: shortenId(parsed.documentId),
      chunkId: parsed.chunkId,
    }) as string
  }

  if (parsed.documentId) {
    return i18n.global.t('flow.search.diagnostics.referenceMeta.document', {
      documentId: shortenId(parsed.documentId),
    }) as string
  }

  return i18n.global.t('flow.search.diagnostics.referenceMeta.stored') as string
}

export function isChunkScopedReference(reference: string): boolean {
  return Boolean(parseReference(reference).chunkId)
}

function parseReference(reference: string) {
  const segments = reference.split(':')

  if (segments[0] !== 'document') {
    return { documentId: null, chunkId: null }
  }

  return {
    documentId: segments[1] ?? null,
    chunkId: segments[2] === 'chunk' ? (segments[3] ?? null) : null,
  }
}

function shortenId(value: string) {
  return value.length > 8 ? value.slice(0, 8) : value
}
