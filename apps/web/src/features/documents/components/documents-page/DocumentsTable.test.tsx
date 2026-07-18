import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import i18n from '@/shared/i18n'

import { DocumentsTable } from './DocumentsTable'

describe('DocumentsTable', () => {
  let container: HTMLDivElement
  let root: Root | null

  beforeEach(() => {
    container = document.createElement('div')
    document.body.appendChild(container)
    root = null
  })

  afterEach(async () => {
    if (root) {
      await act(async () => {
        root?.unmount()
      })
    }
    container.remove()
  })

  async function renderTable() {
    await act(async () => {
      root = createRoot(container)
      root.render(
        <DocumentsTable
          documents={[]}
          items={[]}
          locale="en"
          localSort={null}
          onSelectDoc={vi.fn()}
          onToggleLocalSort={vi.fn()}
          onToggleSelection={vi.fn()}
          onToggleSortDirection={vi.fn()}
          pendingUploads={[
            {
              name: 'broken.pdf',
              state: 'error',
              error: 'The upload stream ended before the file body was complete.',
              errorAction: 'Retry the upload or upload the file individually.',
              errorDiagnosticCode: 'invalid_file_body',
              errorDiagnosticMessage: 'invalid file body for broken.pdf',
            },
          ]}
          processingClockMs={Date.parse('2026-04-10T12:00:00Z')}
          selectedDocId={null}
          selectedIds={new Set()}
          selectionMode={false}
          setSelectedIds={vi.fn()}
          showDetailColumns={false}
          sortBy="uploaded_at"
          sortOrder="desc"
          t={i18n.t.bind(i18n)}
        />,
      )
    })
  }

  it('renders pending upload errors with recovery action and secondary diagnostics', async () => {
    await renderTable()

    const row = Array.from(container.querySelectorAll('tr')).find((candidate) =>
      candidate.textContent?.includes('broken.pdf'),
    )
    const cells = Array.from(row?.querySelectorAll('td') ?? [])
    const statusCell = row?.querySelector('td:last-child')

    expect(row).toBeTruthy()
    expect(cells).toHaveLength(3)
    expect(cells[1]?.getAttribute('colspan')).toBe('2')
    expect(statusCell?.textContent).toContain(
      'The upload stream ended before the file body was complete.',
    )
    expect(statusCell?.textContent).toContain('Retry the upload or upload the file individually.')
    expect(statusCell?.textContent).not.toContain('invalid_file_body')
    expect(statusCell?.querySelector('[title]')?.getAttribute('title')).toContain(
      'invalid_file_body',
    )
  })
})
