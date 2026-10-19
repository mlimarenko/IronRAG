import { render, screen } from '@testing-library/react'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import i18n from '@/shared/i18n'

import { DocumentsFilterNotice } from './DocumentsFilterNotice'

function renderNotice(overrides: Partial<Parameters<typeof DocumentsFilterNotice>[0]> = {}) {
  return render(
    <DocumentsFilterNotice
      bucket="vectorgap"
      canWrite
      onSelectReprocessable={vi.fn()}
      reprocessableCount={0}
      visibleCount={1}
      {...overrides}
    />,
  )
}

describe('DocumentsFilterNotice', () => {
  beforeEach(async () => {
    await i18n.changeLanguage('en')
  })

  it('explains the vector gap and offers to select the reprocessable documents', () => {
    const onSelectReprocessable = vi.fn()
    renderNotice({ reprocessableCount: 2, visibleCount: 3, onSelectReprocessable })

    expect(screen.getByText('Documents missing their semantic index')).toBeInTheDocument()
    expect(screen.getByText(/do not participate in semantic search/)).toBeInTheDocument()
    expect(screen.getByText('Documents on this page: 3')).toBeInTheDocument()

    screen.getByRole('button', { name: 'Select 2 to reprocess' }).click()
    expect(onSelectReprocessable).toHaveBeenCalledTimes(1)
  })

  it('explains the graph gap and says when nothing on the page can be reprocessed', () => {
    renderNotice({ bucket: 'graphgap', reprocessableCount: 0, visibleCount: 1 })

    expect(screen.getByText('Ready documents without fact relationships')).toBeInTheDocument()
    expect(
      screen.getByText('No document on this page can be reprocessed right now.'),
    ).toBeInTheDocument()
    expect(screen.queryByRole('button')).not.toBeInTheDocument()
  })

  it('explains a healthy complete-graph filter without a reprocess action', () => {
    renderNotice({ bucket: 'graphready', reprocessableCount: 4, visibleCount: 4 })

    expect(screen.getByText('Documents with fact relationships')).toBeInTheDocument()
    expect(screen.queryByRole('button')).not.toBeInTheDocument()
  })

  it('keeps the explanation but hides the action from a read-only reader', () => {
    renderNotice({ canWrite: false, reprocessableCount: 2, visibleCount: 2 })

    expect(screen.getByText('Documents missing their semantic index')).toBeInTheDocument()
    expect(screen.queryByRole('button')).not.toBeInTheDocument()
  })
})
