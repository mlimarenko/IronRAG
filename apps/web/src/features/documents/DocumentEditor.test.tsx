import { cleanup, render, screen } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import i18n from '@/shared/i18n'
import type { DocumentItem } from '@/shared/types'

import { DocumentEditor } from './DocumentEditor'

const webDocument: DocumentItem = {
  id: 'document-1',
  documentState: 'active',
  fileName: 'https://example.test/guide',
  fileType: 'html',
  fileSize: 512,
  uploadedAt: '2026-08-11T08:00:00Z',
  cost: null,
  status: 'ready',
  readiness: 'graph_ready',
  sourceKind: 'web_page',
}

describe('DocumentEditor', () => {
  beforeEach(async () => {
    await i18n.changeLanguage('ru')
  })

  afterEach(async () => {
    cleanup()
    await i18n.changeLanguage('en')
  })

  it('shows the localized document type instead of the raw web format', () => {
    render(
      <DocumentEditor
        document={webDocument}
        error={null}
        loading={false}
        markdown="Содержимое страницы"
        onOpenChange={vi.fn()}
        onSave={vi.fn()}
        open
        readOnly={false}
        saving={false}
      />,
    )

    expect(screen.getByText('Веб-страница')).toBeInTheDocument()
    expect(screen.queryByText('HTML')).not.toBeInTheDocument()
  })
})
