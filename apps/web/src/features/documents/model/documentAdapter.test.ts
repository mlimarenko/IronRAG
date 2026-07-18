import { describe, expect, it } from 'vitest'

import i18n from '@/shared/i18n'
import type { DocumentListItem } from '@/shared/api/documents'
import type { DocumentItem } from '@/shared/types'

import {
  formatDocumentTypeLabel,
  getDocumentProcessingDurationMs,
  mapListItem,
} from './documentAdapter'

describe('formatDocumentTypeLabel', () => {
  it('renders a canonical web page label for web-ingested documents', () => {
    expect(formatDocumentTypeLabel('php', 'web_page', i18n.t.bind(i18n))).toBe('Web page')
  })

  it('infers a web page label from URL-backed documents even when sourceKind is missing', () => {
    expect(
      formatDocumentTypeLabel('file', undefined, i18n.t.bind(i18n), {
        fileName: 'https://passport.yandex.ru/showcaptcha?mt=1',
      }),
    ).toBe('Web page')
  })

  it('keeps extension-driven labels for uploaded documents', () => {
    expect(formatDocumentTypeLabel('xlsx', 'upload', i18n.t.bind(i18n))).toBe('XLSX')
  })
})

describe('getDocumentProcessingDurationMs', () => {
  // Widened so a test can explicitly override a field back to `undefined`
  // (e.g. "never started processing") — `Partial<DocumentItem>` alone
  // rejects an explicit `undefined` value under `exactOptionalPropertyTypes`.
  type DocumentItemOverrides = { [K in keyof DocumentItem]?: DocumentItem[K] | undefined }

  function buildDocument(overrides: DocumentItemOverrides = {}): DocumentItem {
    return {
      id: 'doc-1',
      fileName: 'inventory.xlsx',
      fileType: 'xlsx',
      fileSize: 2048,
      uploadedAt: '2026-04-10T12:00:00Z',
      cost: null,
      status: 'processing',
      readiness: 'processing',
      processingStartedAt: '2026-04-10T12:00:05Z',
      ...overrides,
    } as DocumentItem
  }

  it('ticks from processingStartedAt through now while the worker holds the job', () => {
    const durationMs = getDocumentProcessingDurationMs(
      buildDocument(),
      Date.parse('2026-04-10T12:01:05Z'),
    )

    expect(durationMs).toBe(60_000)
  })

  it('returns null for documents that never started processing', () => {
    const durationMs = getDocumentProcessingDurationMs(
      buildDocument({
        status: 'processing',
        readiness: 'processing',
        processingStartedAt: undefined,
      }),
      Date.parse('2026-04-10T12:05:00Z'),
    )

    expect(durationMs).toBeNull()
  })

  it('keeps ticking for a queued status so stuck jobs show accrued wall-clock time', () => {
    const durationMs = getDocumentProcessingDurationMs(
      buildDocument({ status: 'queued' }),
      Date.parse('2026-04-10T12:05:05Z'),
    )

    expect(durationMs).toBe(300_000)
  })

  it('uses processingFinishedAt once the job has completed', () => {
    const durationMs = getDocumentProcessingDurationMs(
      buildDocument({
        status: 'ready',
        readiness: 'graph_ready',
        processingStartedAt: '2026-04-10T12:00:05Z',
        processingFinishedAt: '2026-04-10T12:00:45Z',
      }),
    )

    expect(durationMs).toBe(40_000)
  })

  it('clamps inverted timestamps instead of returning a negative duration', () => {
    const durationMs = getDocumentProcessingDurationMs(
      buildDocument({
        status: 'ready',
        readiness: 'graph_ready',
        processingStartedAt: '2026-04-10T12:05:00Z',
        processingFinishedAt: '2026-04-10T12:04:00Z',
      }),
    )

    expect(durationMs).toBe(0)
  })
})

describe('mapListItem', () => {
  // Widened for the same reason as `DocumentItemOverrides` above — some
  // tests override a field back to `undefined`/omit it, which
  // `Partial<DocumentListItem>` alone rejects under
  // `exactOptionalPropertyTypes`.
  type DocumentListItemOverrides = {
    [K in keyof DocumentListItem]?: DocumentListItem[K] | undefined
  }

  function buildRaw(overrides: DocumentListItemOverrides = {}): DocumentListItem {
    return {
      id: 'doc-1',
      libraryId: 'lib-1',
      workspaceId: 'ws-1',
      externalKey: 'doc-1',
      fileName: 'inventory.xlsx',
      fileType: 'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet',
      fileSize: 2048,
      uploadedAt: '2026-04-10T12:00:00Z',
      documentState: 'active',
      status: 'ready',
      readiness: 'graph_ready',
      stage: 'finalizing',
      retryable: false,
      sourceKind: 'upload',
      cost: '0',
      costCurrencyCode: 'USD',
      ...overrides,
    } as DocumentListItem
  }

  it('passes server-derived status and readiness through verbatim', () => {
    const doc = mapListItem(
      buildRaw({ status: 'processing', readiness: 'processing' }),
      i18n.t.bind(i18n),
    )

    expect(doc.status).toBe('processing')
    expect(doc.readiness).toBe('processing')
  })

  it('derives the file extension from the file name first', () => {
    const doc = mapListItem(
      buildRaw({ fileName: 'report.pdf', fileType: 'application/pdf' }),
      i18n.t.bind(i18n),
    )

    expect(doc.fileType).toBe('pdf')
  })

  it('falls back to the MIME subtype when the file name has no extension', () => {
    const doc = mapListItem(
      buildRaw({ fileName: 'untitled', fileType: 'text/markdown' }),
      i18n.t.bind(i18n),
    )

    expect(doc.fileType).toBe('markdown')
  })

  it('decodes URL-encoded names so the UI shows real characters for web-captured docs', () => {
    const doc = mapListItem(
      buildRaw({
        fileName: 'M%C3%BCller%20Handbook%20v2.pdf',
        fileType: 'application/pdf;charset=utf-8',
        sourceKind: 'web_page',
      }),
      i18n.t.bind(i18n),
    )

    expect(doc.fileName.startsWith('Müller Handbook')).toBe(true)
    expect(doc.fileName).toContain('v2')
    expect(doc.fileType).toBe('pdf')
  })

  it('does not derive a fake extension from a bare URL host or path', () => {
    const doc = mapListItem(
      buildRaw({
        fileName: 'https://passport.yandex.ru/showcaptcha?mt=1',
        fileType: '',
        sourceKind: 'web_page',
      }),
      i18n.t.bind(i18n),
    )

    expect(doc.fileType).toBe('file')
  })

  it('falls back safely when the backend has not resolved mime type or file size yet', () => {
    const doc = mapListItem(
      buildRaw({
        fileName: 'untitled',
        fileType: null,
        fileSize: null,
        status: 'processing',
        readiness: 'processing',
      }),
      i18n.t.bind(i18n),
    )

    expect(doc.fileType).toBe('file')
    expect(doc.fileSize).toBe(0)
  })

  it('surfaces retryable on failed jobs so the inspector can offer a retry', () => {
    const doc = mapListItem(
      buildRaw({ status: 'failed', readiness: 'failed', retryable: true }),
      i18n.t.bind(i18n),
    )

    expect(doc.status).toBe('failed')
    expect(doc.canRetry).toBe(true)
  })

  it('maps list progress and failure fields for the table and inspector', () => {
    const doc = mapListItem(
      buildRaw({
        status: 'processing',
        readiness: 'processing',
        stage: 'extract_graph',
        progressPercent: 76.4,
      }),
      i18n.t.bind(i18n),
    )

    expect(doc.progressPercent).toBe(76)
    expect(doc.stage).toBe('Extracting graph')
  })

  it('keeps backend failure details as diagnostics behind a known failed-document reason', () => {
    const doc = mapListItem(
      buildRaw({
        status: 'failed',
        readiness: 'failed',
        failureCode: 'parser_failed',
        failureMessage: 'Parser failed on page 2',
      }),
      i18n.t.bind(i18n),
    )

    expect(doc.failureCode).toBe('parser_failed')
    expect(doc.failureMessage).toBe('Parser failed on page 2')
    expect(doc.statusReason).toBe('The document parser could not extract usable content.')
    expect(doc.failureNotice?.diagnosticMessage).toBe('Parser failed on page 2')
  })

  it('maps upload extraction failures to operator guidance with diagnostics', () => {
    const doc = mapListItem(
      buildRaw({
        status: 'failed',
        readiness: 'failed',
        stage: 'extract_content',
        failureCode: 'upload_extraction_failed',
        failureMessage: 'Upload extraction failed',
      }),
      i18n.t.bind(i18n),
    )

    expect(doc.statusReason).toContain('could not be extracted')
    expect(doc.failureNotice?.summary).toContain('could not be extracted')
    expect(doc.failureNotice?.action).toContain('Retry the upload')
    expect(doc.failureNotice?.diagnosticCode).toBe('upload_extraction_failed')
    expect(doc.failureNotice?.diagnosticMessage).toBe('Upload extraction failed')
  })

  it('returns null for non-numeric list cost values', () => {
    const doc = mapListItem(buildRaw({ cost: '' }), i18n.t.bind(i18n))

    expect(doc.cost).toBeNull()
  })
})
