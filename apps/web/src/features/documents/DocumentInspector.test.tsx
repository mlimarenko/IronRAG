import { render, screen } from '@testing-library/react'
import { MemoryRouter } from 'react-router-dom'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import i18n from '@/shared/i18n'
import type { DocumentItem } from '@/shared/types'

import { DocumentInspector } from './DocumentInspector'

const hooks = vi.hoisted(() => ({
  useDocumentEditor: vi.fn(),
  useDocumentInspectorController: vi.fn(),
}))

vi.mock('./components/documents-page/useDocumentInspectorController', () => ({
  useDocumentInspectorController: hooks.useDocumentInspectorController,
}))

vi.mock('./components/editor/useDocumentEditor', () => ({
  useDocumentEditor: hooks.useDocumentEditor,
}))

type DocumentOverrides = { [K in keyof DocumentItem]?: DocumentItem[K] | undefined }

function buildDocument(overrides: DocumentOverrides = {}): DocumentItem {
  return {
    id: 'document-1',
    documentState: 'active',
    fileName: 'runbook.pdf',
    fileType: 'pdf',
    fileSize: 2048,
    uploadedAt: '2026-08-10T10:00:00Z',
    cost: null,
    status: 'processing',
    readiness: 'processing',
    progressPercent: 37,
    ...overrides,
  } as DocumentItem
}

function renderInspector(document: DocumentItem, canEdit = false) {
  return render(
    <MemoryRouter>
      <DocumentInspector
        activateListPollGrace={vi.fn()}
        canDelete={false}
        canEdit={canEdit}
        document={document}
        errorMessage={(_error, fallback) => fallback}
        fetchSelectedDetail={vi.fn().mockResolvedValue(undefined)}
        lifecycle={null}
        loadFirstPage={vi.fn().mockResolvedValue(undefined)}
        locale="en"
        onClose={vi.fn()}
        onDocumentHintUpdated={vi.fn()}
        onSelectDocument={vi.fn()}
      />
    </MemoryRouter>,
  )
}

describe('DocumentInspector', () => {
  beforeEach(async () => {
    vi.clearAllMocks()
    await i18n.changeLanguage('en')
    i18n.addResource(
      'en',
      'translation',
      'documents.retryUnavailable',
      'Processing cannot be retried for this document in its current state.',
    )
    hooks.useDocumentInspectorController.mockReturnValue({
      download: vi.fn(),
      retry: vi.fn(),
    })
    hooks.useDocumentEditor.mockReturnValue({
      editorDocument: null,
      editorError: null,
      editorLoading: false,
      editorMarkdown: '',
      editorOpen: false,
      editorReadOnly: false,
      editorSaving: false,
      handleEditorOpenChange: vi.fn(),
      openEditor: vi.fn(),
      saveEditor: vi.fn(),
    })
  })

  it('shows a localized pipeline stage without exposing its internal identifier', () => {
    renderInspector(buildDocument({ stage: 'extract_graph' }))

    expect(screen.getByText('Extracting graph · 37%')).toBeInTheDocument()
    expect(screen.getByRole('progressbar')).toHaveAccessibleName('Extracting graph')
    expect(screen.queryByText(/extract_graph/)).not.toBeInTheDocument()
  })

  it('does not repeat the processing status when no stage is available', () => {
    renderInspector(buildDocument())

    expect(screen.getAllByText('Processing')).toHaveLength(1)
    expect(screen.getByText('37%')).toBeInTheDocument()
    expect(screen.getByRole('progressbar')).toHaveAccessibleName('Pipeline')
  })

  it('explains why retry is disabled', () => {
    renderInspector(buildDocument({ canRetry: false }), true)

    const retryButton = screen.getByText('Retry Processing').closest('button')
    expect(retryButton).toBeDisabled()
    expect(retryButton).toHaveAttribute(
      'title',
      'Processing cannot be retried for this document in its current state.',
    )
  })

  it('offers reprocess for a ready document the server marks non-retryable', () => {
    // `retryable` answers "can a failed attempt be retried". A ready document
    // has no failed attempt to retry, which is not the same question as "can
    // the index be rebuilt" — the server accepts a reprocess for it.
    renderInspector(
      buildDocument({ status: 'ready', readiness: 'graph_sparse', canRetry: false }),
      true,
    )

    const reprocessButton = screen.getByText('Reprocess').closest('button')
    expect(reprocessButton).not.toBeDisabled()
    expect(reprocessButton).not.toHaveAttribute('title')
  })

  it('keeps a non-retryable terminal document disabled', () => {
    // A canceled attempt covers the superseded case; `retryable === false`
    // means the server will not accept a rerun, so the control stays off.
    renderInspector(buildDocument({ status: 'canceled', canRetry: false }), true)

    const retryButton = screen.getByText('Retry Processing').closest('button')
    expect(retryButton).toBeDisabled()
  })

  it('does not offer reprocessing for a deleted ready document', () => {
    renderInspector(
      buildDocument({ documentState: 'deleted', status: 'ready', canRetry: true }),
      true,
    )
    expect(screen.getByText('Reprocess').closest('button')).toBeDisabled()
  })
})
