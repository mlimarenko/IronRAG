import { fireEvent, render, screen } from '@testing-library/react'
import { describe, expect, it, vi } from 'vitest'

import i18n from '@/shared/i18n'
import type { DocumentItem } from '@/shared/types'

import { DocumentsTable } from './DocumentsTable'

const document: DocumentItem = {
  id: 'document-alpha',
  fileName: 'Alpha report.pdf',
  fileType: 'pdf',
  fileSize: 42,
  uploadedAt: '2026-04-10T12:00:00Z',
  cost: null,
  status: 'ready',
  readiness: 'readable',
}

function renderDocumentsTable(onSelectDoc = vi.fn()) {
  render(
    <DocumentsTable
      documents={[document]}
      items={[document]}
      locale="en"
      localSort={null}
      onSelectDoc={onSelectDoc}
      onToggleLocalSort={vi.fn()}
      onToggleSelection={vi.fn()}
      onToggleSortDirection={vi.fn()}
      pendingUploads={[]}
      processingClockMs={Date.parse('2026-04-10T12:00:00Z')}
      selectedDocId="document-alpha"
      selectedIds={new Set()}
      selectionMode={false}
      setSelectedIds={vi.fn()}
      showDetailColumns={false}
      sortBy="uploaded_at"
      sortOrder="desc"
      t={i18n.t.bind(i18n)}
    />,
  )
}

describe('DocumentsTable', () => {
  it('selects a document through its native action buttons', () => {
    const onSelectDoc = vi.fn()

    renderDocumentsTable(onSelectDoc)

    const sortHeader = screen.getByRole('columnheader', { name: /Uploaded/ })
    expect(sortHeader).toHaveAttribute('aria-sort', 'descending')
    expect(screen.getByRole('button', { name: 'Uploaded' })).not.toHaveAttribute('aria-sort')
    expect(screen.getByRole('columnheader', { name: /Name/ })).not.toHaveAttribute('aria-sort')

    const documentButton = screen.getByRole('button', { name: 'Alpha report.pdf Ready' })
    expect(documentButton).toHaveAttribute('aria-current', 'true')

    fireEvent.click(documentButton)

    expect(onSelectDoc).toHaveBeenCalledWith(document)
  })
})
