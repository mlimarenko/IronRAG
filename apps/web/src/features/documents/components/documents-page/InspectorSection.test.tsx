import userEvent from '@testing-library/user-event'
import { MemoryRouter } from 'react-router-dom'
import { render, screen } from '@testing-library/react'
import { describe, expect, it, vi } from 'vitest'

import i18n from '@/shared/i18n'
import type { DocumentItem } from '@/shared/types'

import { InspectorSection } from './InspectorSection'

const SELECTED_DOCUMENT: DocumentItem = {
  id: 'document-alpha',
  fileName: 'alpha.pdf',
  fileType: 'application/pdf',
  fileSize: 128,
  uploadedAt: '2026-04-10T12:00:00Z',
  cost: null,
  status: 'ready',
  readiness: 'readable',
}

describe('InspectorSection', () => {
  it('keeps the replace-file input reachable by keyboard', async () => {
    const user = userEvent.setup()
    render(
      <MemoryRouter>
        <InspectorSection
          activateListPollGrace={vi.fn()}
          canDelete
          canEdit
          clearSelectedDoc={vi.fn()}
          documentHintEditable={false}
          errorMessage={(_error, fallback) => fallback}
          fetchSelectedDetail={vi.fn()}
          inspectorLifecycle={null}
          loadFirstPage={vi.fn()}
          locale="en"
          selectedDoc={SELECTED_DOCUMENT}
          selectDoc={vi.fn()}
          selectionMode={false}
          t={i18n.t.bind(i18n)}
          updateDocumentHintLocally={vi.fn()}
          updateSearchParamState={vi.fn()}
        />
      </MemoryRouter>,
    )

    await user.click(screen.getByRole('button', { name: 'Replace File' }))

    const fileInput = screen.getByLabelText(/Drop a file or click to browse/)
    expect(fileInput).toHaveAttribute('type', 'file')
    expect(fileInput).toHaveFocus()
    expect(fileInput.parentElement).toHaveClass('focus-within:ring-2', 'focus-within:ring-ring')
  })
})
