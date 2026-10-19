import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { act, renderHook } from '@testing-library/react'
import type { ReactNode } from 'react'
import { createElement } from 'react'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import type { DocumentItem } from '@/shared/types'

const mocks = vi.hoisted(() => ({
  getSource: vi.fn(),
}))

vi.mock('@/shared/api', () => ({
  documentsApi: {
    getSourceBlob: (documentId: string, revisionId?: string) =>
      mocks.getSource(documentId, revisionId, 'blob'),
  },
  queries: {
    listContentRevisionsOptions: (options: { path: { documentId: string } }) => ({
      queryKey: ['revisions', options.path.documentId],
      queryFn: () => Promise.reject(new Error('not exercised by this test')),
    }),
    listContentPreparedSegmentsOptions: (options: { path: { documentId: string } }) => ({
      queryKey: ['segments', options.path.documentId],
      queryFn: () => Promise.reject(new Error('not exercised by this test')),
    }),
    listContentTechnicalFactsOptions: (options: { path: { documentId: string } }) => ({
      queryKey: ['facts', options.path.documentId],
      queryFn: () => Promise.reject(new Error('not exercised by this test')),
    }),
  },
}))

vi.mock('sonner', () => ({
  toast: {
    error: vi.fn(),
    success: vi.fn(),
    warning: vi.fn(),
  },
}))

const { toast } = await import('sonner')
const { useDocumentInspectorController, revisionFileName } =
  await import('./useDocumentInspectorController')

describe('revisionFileName', () => {
  it('inserts the revision marker before the extension', () => {
    expect(revisionFileName('report.pdf', 3)).toBe('report (v3).pdf')
  })

  it('appends the revision marker when the file has no extension', () => {
    expect(revisionFileName('README', 2)).toBe('README (v2)')
  })

  it('treats a leading dot as a hidden-file name, not an extension', () => {
    expect(revisionFileName('.gitignore', 1)).toBe('.gitignore (v1)')
  })
})

const document: DocumentItem = {
  id: 'doc-1',
  documentState: 'active',
  fileName: 'handbook.txt',
  fileType: 'txt',
  fileSize: 42,
  uploadedAt: '2026-08-20T09:00:00Z',
  cost: null,
  status: 'ready',
  readiness: 'graph_ready',
}

function renderController(queryClient = new QueryClient(), selectedDocument = document) {
  function Wrapper({ children }: Readonly<{ children: ReactNode }>) {
    return createElement(QueryClientProvider, { client: queryClient }, children)
  }
  return renderHook(
    () =>
      useDocumentInspectorController({
        activateListPollGrace: () => undefined,
        document: selectedDocument,
        errorMessage: (error: unknown, fallback: string) =>
          error instanceof Error ? error.message : fallback,
        fetchSelectedDetail: async () => undefined,
        loadFirstPage: async () => undefined,
        onClose: () => undefined,
        onDocumentHintUpdated: () => undefined,
        section: 'pipeline',
        t: ((key: string) => key) as never,
      }),
    { wrapper: Wrapper },
  )
}

// Binary parsing is owned by documentsApi, including text/plain sources.
// Keep the controller fixture sensitive to receiving a string instead of a Blob.
function respondAsTextSource(body: string) {
  mocks.getSource.mockImplementation((_documentId, _revisionId, parseAs) =>
    Promise.resolve(parseAs === 'blob' ? new Blob([body], { type: 'text/plain' }) : body),
  )
}

describe('useDocumentInspectorController download', () => {
  beforeEach(() => {
    mocks.getSource.mockReset()
    vi.mocked(toast.error).mockReset()
  })

  it('delegates the download name to the browser when publication metadata is absent', async () => {
    respondAsTextSource('body')
    const queryClient = new QueryClient()
    const createObjectURL = vi.spyOn(URL, 'createObjectURL').mockReturnValue('blob:unnamed')
    const revokeObjectURL = vi.spyOn(URL, 'revokeObjectURL').mockImplementation(() => undefined)
    let downloadAttribute: string | null | undefined
    const click = vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(function (
      this: HTMLAnchorElement,
    ) {
      downloadAttribute = this.getAttribute('download')
    })
    const { result, unmount } = renderController(queryClient, { ...document, fileName: null })
    try {
      await act(async () => {
        await result.current.download({ id: 'rev-1', revisionNumber: 1 })
      })
      expect(downloadAttribute).toBe('')
      expect(toast.error).not.toHaveBeenCalled()
    } finally {
      unmount()
      queryClient.clear()
      createObjectURL.mockRestore()
      revokeObjectURL.mockRestore()
      click.mockRestore()
    }
  })

  it('revalidates source access instead of downloading a fresh cached blob after visibility denial', async () => {
    const queryClient = new QueryClient({
      defaultOptions: { queries: { retry: false, staleTime: Infinity } },
    })
    queryClient.setQueryData(
      ['document-source', 'doc-1', undefined, 'blob'],
      new Blob(['previously readable source'], { type: 'text/plain' }),
    )
    mocks.getSource.mockRejectedValue(new Error('source is no longer readable'))
    const createObjectURL = vi.spyOn(URL, 'createObjectURL').mockReturnValue('blob:cached-source')
    const click = vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(() => undefined)
    const { result, unmount } = renderController(queryClient)

    try {
      await act(async () => {
        await result.current.download()
      })

      expect(mocks.getSource).toHaveBeenCalledWith('doc-1', undefined, 'blob')
      expect(createObjectURL).not.toHaveBeenCalled()
      expect(click).not.toHaveBeenCalled()
      expect(toast.error).toHaveBeenCalledWith('documents.failedToLoad')
    } finally {
      unmount()
      queryClient.clear()
      createObjectURL.mockRestore()
      click.mockRestore()
    }
  })

  it('hands URL.createObjectURL a Blob for a text/plain source, not the raw string (IRG-78)', async () => {
    respondAsTextSource('line one\nline two\n')
    const createObjectURL = vi.spyOn(URL, 'createObjectURL').mockReturnValue('blob:handbook')
    const revokeObjectURL = vi.spyOn(URL, 'revokeObjectURL').mockImplementation(() => undefined)
    let clickedDownloadName: string | undefined
    const click = vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(function (
      this: HTMLAnchorElement,
    ) {
      clickedDownloadName = this.download
    })

    const { result } = renderController()
    await act(async () => {
      await result.current.download()
    })

    expect(createObjectURL).toHaveBeenCalledTimes(1)
    expect(createObjectURL.mock.calls[0]?.[0]).toBeInstanceOf(Blob)
    expect(clickedDownloadName).toBe('handbook.txt')
    expect(revokeObjectURL).toHaveBeenCalledWith('blob:handbook')
    expect(toast.error).not.toHaveBeenCalled()

    createObjectURL.mockRestore()
    revokeObjectURL.mockRestore()
    click.mockRestore()
  })

  it('applies the same fix to a revision download and names the file after the revision', async () => {
    respondAsTextSource('revision body\n')
    const createObjectURL = vi.spyOn(URL, 'createObjectURL').mockReturnValue('blob:revision')
    const revokeObjectURL = vi.spyOn(URL, 'revokeObjectURL').mockImplementation(() => undefined)
    let clickedDownloadName: string | undefined
    const click = vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(function (
      this: HTMLAnchorElement,
    ) {
      clickedDownloadName = this.download
    })

    const { result } = renderController()
    await act(async () => {
      await result.current.download({ id: 'rev-1', revisionNumber: 2 })
    })

    expect(mocks.getSource).toHaveBeenCalledWith('doc-1', 'rev-1', 'blob')
    expect(createObjectURL.mock.calls[0]?.[0]).toBeInstanceOf(Blob)
    expect(clickedDownloadName).toBe('handbook (v2).txt')
    expect(toast.error).not.toHaveBeenCalled()

    createObjectURL.mockRestore()
    revokeObjectURL.mockRestore()
    click.mockRestore()
  })
})
