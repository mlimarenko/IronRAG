import { render, screen } from '@testing-library/react'
import { createRef, type ComponentProps } from 'react'
import { describe, expect, it, vi } from 'vitest'

import { UploadDialog } from './UploadDialog'

type UploadDialogController = ComponentProps<typeof UploadDialog>['controller']

function createController(overrides: Partial<UploadDialogController> = {}): UploadDialogController {
  return {
    cancelUploadDialog: vi.fn(),
    confirmUploadDialog: vi.fn(),
    dragOver: false,
    dropTargetProps: {
      onDragLeave: vi.fn(),
      onDragOver: vi.fn(),
      onDrop: vi.fn(),
    },
    duplicateConflict: null,
    fileInputRef: createRef<HTMLInputElement>(),
    folderInputRef: createRef<HTMLInputElement>(),
    handleFileSelect: vi.fn(),
    handleFolderSelect: vi.fn(),
    pendingUploads: [],
    resolveDuplicate: vi.fn(),
    setUploadDialogHint: vi.fn(),
    uploadDialogFileCount: 2,
    uploadDialogHint: '',
    uploadDialogOpen: true,
    ...overrides,
  }
}

describe('UploadDialog', () => {
  it('stacks upload choices below the medium breakpoint', () => {
    render(<UploadDialog controller={createController()} />)

    expect(
      screen.getByRole('button', { name: /choose files/i }).closest('.upload-choice'),
    ).toHaveClass('max-md:grid-cols-1!')
  })

  it('shows duplicate context without repeating the conflict or document name', () => {
    const file = new File([new Uint8Array(2048)], 'manual.pdf', {
      type: 'application/pdf',
    })
    render(
      <UploadDialog
        controller={createController({
          duplicateConflict: {
            candidate: { file, name: file.name },
            documentHint: '',
            existingDocId: 'document-1',
            remaining: [],
          },
          uploadDialogOpen: false,
        })}
      />,
    )

    expect(screen.getAllByText(/manual\.pdf/)).toHaveLength(1)
    expect(screen.queryByText('This content already exists')).not.toBeInTheDocument()
    expect(screen.getByText('2.0 KB')).toBeInTheDocument()
  })
})
