import { File as NodeFile } from 'node:buffer'

import { afterEach, describe, expect, it, vi } from 'vitest'

import { SnapshotImportTimeoutError, documentsApi, librarySnapshotApi } from './documents'
import { Content } from './generated'
import { client } from './generated/client.gen'
import {
  markSessionSignedIn,
  markSessionSignedOut,
  subscribeToSessionExpired,
} from './sessionLifecycle'

describe('documentsApi', () => {
  afterEach(() => {
    client.setConfig({ baseUrl: '' })
    vi.restoreAllMocks()
  })

  describe('document source text and the session', () => {
    function unauthorized() {
      return new Response('unauthorized', { status: 401 })
    }

    async function expiriesWhileFetching(sourceHref: string): Promise<number> {
      vi.spyOn(globalThis, 'fetch').mockResolvedValue(unauthorized())
      const expired = vi.fn()
      const unsubscribe = subscribeToSessionExpired(expired)
      markSessionSignedIn()
      try {
        await expect(documentsApi.getSourceText(sourceHref)).rejects.toMatchObject({ status: 401 })
        return expired.mock.calls.length
      } finally {
        unsubscribe()
        markSessionSignedOut()
      }
    }

    it('treats a 401 from this application as the session ending', async () => {
      expect(await expiriesWhileFetching('/v1/content/documents/doc-1/source')).toBe(1)
    })

    it('does not sign the user out because another site refused the source', async () => {
      // Web-ingested documents carry the origin site's own URL. A 401 from
      // example.com says nothing about this application's session.
      expect(await expiriesWhileFetching('https://example.com/guide')).toBe(0)
    })
  })

  it('reads text sources as uncached blobs and propagates a later visibility denial', async () => {
    client.setConfig({ baseUrl: 'http://localhost' })
    const requests: Request[] = []
    vi.spyOn(globalThis, 'fetch').mockImplementation(
      async (input: RequestInfo | URL, init?: RequestInit) => {
        requests.push(input instanceof Request ? input : new Request(input, init))
        return requests.length === 1
          ? new Response('readable source', { headers: { 'Content-Type': 'text/plain' } })
          : new Response(JSON.stringify({ detail: 'Source unavailable', code: 'source_missing' }), {
              status: 404,
              headers: { 'Content-Type': 'application/json' },
            })
      },
    )

    const source = await documentsApi.getSourceBlob('doc-1', 'revision-1')
    expect(source).toBeInstanceOf(Blob)
    await expect(source.text()).resolves.toBe('readable source')
    await expect(documentsApi.getSourceBlob('doc-1', 'revision-1')).rejects.toMatchObject({
      status: 404,
    })
    expect(requests).toHaveLength(2)
    for (const request of requests) {
      expect(request.cache).toBe('no-store')
      expect(request.url).toBe(
        'http://localhost/v1/content/documents/doc-1/source?revisionId=revision-1',
      )
    }
  })

  it('patches document hints through the generated metadata operation', async () => {
    const patchMock = vi.spyOn(Content, 'patchContentDocumentMetadata').mockResolvedValue({
      data: { activeRevision: { document_hint: null } },
    } as Awaited<ReturnType<typeof Content.patchContentDocumentMetadata>>)

    const result = await documentsApi.updateDocumentHint('doc-1', null)

    expect(result).toBeNull()
    expect(patchMock).toHaveBeenCalledWith({
      path: { documentId: 'doc-1' },
      body: { documentHint: null },
    })
  })

  it('rejects a metadata response without the required saved hint field', async () => {
    vi.spyOn(Content, 'patchContentDocumentMetadata').mockResolvedValue({
      data: { activeRevision: {} },
    } as Awaited<ReturnType<typeof Content.patchContentDocumentMetadata>>)

    await expect(documentsApi.updateDocumentHint('doc-1', ' Client hint ')).rejects.toThrow(
      'document hint missing from saved response',
    )
  })

  it('returns the saved hint from the patch response', async () => {
    vi.spyOn(Content, 'patchContentDocumentMetadata').mockResolvedValue({
      data: { activeRevision: { document_hint: 'Server hint' } },
    } as Awaited<ReturnType<typeof Content.patchContentDocumentMetadata>>)

    await expect(documentsApi.updateDocumentHint('doc-1', ' Client hint ')).resolves.toBe(
      'Server hint',
    )
  })

  it('uploads a file through the primary library document creation route', async () => {
    const file = new File(['content'], 'original.txt', { type: 'text/plain' })
    const postMock = vi.spyOn(client, 'post').mockResolvedValue({
      data: { document: {}, mutation: {} },
    })

    await documentsApi.upload('019e37a5-6295-7022-b0ec-cdc0bcd03715', file, {
      documentHint: 'Brief context',
      externalKey: 'source/original.txt',
      fileName: 'renamed.txt',
      title: 'Readable title',
    })

    const request = postMock.mock.calls[0]?.[0]
    if (!request) throw new Error('expected a captured client request')
    expect(request.url).toBe('/v1/content/libraries/{libraryId}/documents')
    expect(request.path).toEqual({ libraryId: '019e37a5-6295-7022-b0ec-cdc0bcd03715' })
    expect(request.headers).toEqual({ 'Content-Type': null })
    // A JSON-serialized FormData goes out as text/plain and the API answers 415.
    expect(request.bodySerializer).toBeNull()
    const body = request.body as FormData
    expect(body.get('library_id')).toBeNull()
    expect(body.get('document_hint')).toBe('Brief context')
    expect(body.get('external_key')).toBe('source/original.txt')
    expect(body.get('title')).toBe('Readable title')
    expect((body.get('file') as File).name).toBe('renamed.txt')
  })

  it('creates a text revision through the current revision endpoint', async () => {
    client.setConfig({ baseUrl: 'http://localhost' })
    const requests: Array<{ input: RequestInfo | URL; init?: RequestInit | undefined }> = []
    vi.spyOn(globalThis, 'fetch').mockImplementation(
      async (input: RequestInfo | URL, init?: RequestInit) => {
        requests.push({ input, init })
        return new Response(JSON.stringify({}), {
          headers: { 'Content-Type': 'application/json' },
          status: 202,
        })
      },
    )

    await documentsApi.edit('019e37a5-6295-7022-b0ec-cdc0bcd03715', 'Updated content')

    expect(requests).toHaveLength(1)
    const request = requests[0]
    if (!request) throw new Error('expected a captured fetch request')
    const sentRequest =
      request.input instanceof Request ? request.input : new Request(request.input, request.init)
    expect(sentRequest.url).toBe(
      'http://localhost/v1/content/documents/019e37a5-6295-7022-b0ec-cdc0bcd03715/revisions',
    )
    await expect(sentRequest.text()).resolves.toBe(
      JSON.stringify({ mode: 'replace', markdown: 'Updated content' }),
    )
  })

  it('replaces a file through the current revision endpoint', async () => {
    const file = new File(['content'], 'replacement.txt', { type: 'text/plain' })
    const postMock = vi.spyOn(client, 'post').mockResolvedValue({ data: {} })

    await documentsApi.replace('019e37a5-6295-7022-b0ec-cdc0bcd03715', file)

    const request = postMock.mock.calls[0]?.[0]
    if (!request) throw new Error('expected a captured client request')
    expect(request.url).toBe('/v1/content/documents/{documentId}/revisions')
    expect(request.path).toEqual({ documentId: '019e37a5-6295-7022-b0ec-cdc0bcd03715' })
    expect(request.headers).toEqual({ 'Content-Type': null })
    // A JSON-serialized FormData goes out as text/plain and the API answers 415.
    expect(request.bodySerializer).toBeNull()
    const uploadedFile = (request.body as FormData).get('file')
    expect(uploadedFile).toBeInstanceOf(File)
    expect((uploadedFile as File).name).toBe(file.name)
  })

  it('preserves an explicit empty ID constraint while forwarding populated supported filters', async () => {
    const listMock = vi.spyOn(Content, 'listContentDocuments').mockResolvedValue({
      data: { items: [], nextCursor: null },
    } as unknown as Awaited<ReturnType<typeof Content.listContentDocuments>>)

    await documentsApi.list({
      libraryId: 'library-1',
      cursor: 'cursor-1',
      limit: 25,
      status: [],
      ids: [],
      includeDeleted: false,
      includeTotal: true,
    })
    await documentsApi.list({
      libraryId: 'library-1',
      status: ['ready', 'failed'],
      ids: ['document-1', 'document-2'],
    })

    expect(listMock).toHaveBeenNthCalledWith(1, {
      path: { libraryId: 'library-1' },
      query: { cursor: 'cursor-1', limit: 25, includeDeleted: false, includeTotal: true, ids: '' },
    })
    expect(listMock).toHaveBeenNthCalledWith(2, {
      path: { libraryId: 'library-1' },
      query: { status: 'ready,failed', ids: 'document-1,document-2' },
    })
  })

  it('forwards the graph indexing invariants to the list endpoint', async () => {
    const listMock = vi.spyOn(Content, 'listContentDocuments').mockResolvedValue({
      data: { items: [], nextCursor: null },
    } as unknown as Awaited<ReturnType<typeof Content.listContentDocuments>>)

    await documentsApi.list({ libraryId: 'library-1', indexing: 'graph_gap' })
    await documentsApi.list({ libraryId: 'library-1', indexing: 'graph_ready' })
    await documentsApi.list({ libraryId: 'library-1', indexing: 'vector_gap' })

    expect(listMock).toHaveBeenNthCalledWith(1, {
      path: { libraryId: 'library-1' },
      query: { indexing: 'graph_gap' },
    })
    expect(listMock).toHaveBeenNthCalledWith(2, {
      path: { libraryId: 'library-1' },
      query: { indexing: 'graph_ready' },
    })
    expect(listMock).toHaveBeenNthCalledWith(3, {
      path: { libraryId: 'library-1' },
      query: { indexing: 'vector_gap' },
    })
  })

  it('collects prepared-segment pages until the reported total is reached', async () => {
    const listMock = vi
      .spyOn(Content, 'listContentPreparedSegments')
      .mockResolvedValueOnce({
        data: { items: [{ id: 'segment-1' }, { id: 'segment-2' }], total: 3 },
      } as unknown as Awaited<ReturnType<typeof Content.listContentPreparedSegments>>)
      .mockResolvedValueOnce({
        data: { items: [{ id: 'segment-3' }], total: 3 },
      } as unknown as Awaited<ReturnType<typeof Content.listContentPreparedSegments>>)

    const segments = await documentsApi.getAllPreparedSegments('document-1')

    expect(segments).toHaveLength(3)
    expect(listMock).toHaveBeenNthCalledWith(1, {
      path: { documentId: 'document-1' },
      query: { offset: 0, limit: 500 },
    })
    expect(listMock).toHaveBeenNthCalledWith(2, {
      path: { documentId: 'document-1' },
      query: { offset: 2, limit: 500 },
    })
  })

  it('returns structured API errors for rejected source-text responses', async () => {
    vi.spyOn(globalThis, 'fetch').mockResolvedValue(
      new Response(JSON.stringify({ detail: 'Source unavailable', code: 'source_missing' }), {
        headers: { 'Content-Type': 'application/problem+json' },
        status: 404,
      }),
    )

    await expect(documentsApi.getSourceText('/source/document-1')).rejects.toMatchObject({
      status: 404,
      body: { detail: 'Source unavailable', code: 'source_missing', status: 404 },
    })
  })
})

describe('librarySnapshotApi', () => {
  afterEach(() => {
    client.setConfig({ baseUrl: '' })
    vi.restoreAllMocks()
  })

  it('uses a typed timeout failure instead of message classification', () => {
    const error = new SnapshotImportTimeoutError()

    expect(error).toBeInstanceOf(Error)
    expect(error).toBeInstanceOf(SnapshotImportTimeoutError)
  })

  it('sends snapshot imports as the raw archive body', async () => {
    client.setConfig({ baseUrl: 'http://localhost' })
    const payload = [0x28, 0xb5, 0x2f, 0xfd, 0x00, 0x61, 0x72, 0x63]
    const file = new NodeFile([new Uint8Array(payload)], 'snapshot.tar.zst', {
      type: 'application/zstd',
    }) as unknown as File
    const requests: Array<{ input: RequestInfo | URL; init?: RequestInit | undefined }> = []
    const fetchMock = vi
      .spyOn(globalThis, 'fetch')
      .mockImplementation(async (input: RequestInfo | URL, init?: RequestInit) => {
        requests.push({ input, init })
        return new Response(
          JSON.stringify({
            operationId: '019e37a5-6295-7022-b0ec-cdc0bcd03716',
            workspaceId: '019e37a5-6295-7022-b0ec-cdc0bcd03717',
            libraryId: '019e37a5-6295-7022-b0ec-cdc0bcd03715',
            overwriteMode: 'replace',
            archiveBytes: payload.length,
          }),
          {
            headers: { 'Content-Type': 'application/json' },
            status: 202,
          },
        )
      })

    const result = await librarySnapshotApi.import(
      '019e37a5-6295-7022-b0ec-cdc0bcd03715',
      file,
      'replace',
    )

    expect(result).toEqual({
      operationId: '019e37a5-6295-7022-b0ec-cdc0bcd03716',
      workspaceId: '019e37a5-6295-7022-b0ec-cdc0bcd03717',
      libraryId: '019e37a5-6295-7022-b0ec-cdc0bcd03715',
      overwriteMode: 'replace',
      archiveBytes: payload.length,
    })
    expect(fetchMock).toHaveBeenCalledTimes(1)
    expect(requests).toHaveLength(1)
    const request = requests[0]
    if (!request) throw new Error('expected a captured fetch request')
    expect(String(request.input)).toBe(
      'http://localhost/v1/content/libraries/019e37a5-6295-7022-b0ec-cdc0bcd03715/snapshot?overwrite=replace',
    )
    expect(request.init?.method).toBe('POST')
    expect(request.init?.credentials).toBe('include')
    expect(request.init?.headers).toEqual({ 'Content-Type': 'application/zstd' })
    const body = request.init?.body as File
    expect(Array.from(new Uint8Array(await body.arrayBuffer()))).toEqual(payload)
  })

  it('rejects malformed snapshot acceptance payloads at the API boundary', async () => {
    client.setConfig({ baseUrl: 'http://localhost' })
    const file = new NodeFile(['archive'], 'snapshot.tar.zst', {
      type: 'application/zstd',
    }) as unknown as File
    vi.spyOn(globalThis, 'fetch').mockResolvedValue(
      new Response(JSON.stringify({ operationId: 'incomplete' }), {
        headers: { 'Content-Type': 'application/json' },
        status: 202,
      }),
    )

    await expect(librarySnapshotApi.import('library-1', file, 'reject')).rejects.toThrow(
      'snapshot import returned an invalid accepted response',
    )
  })

  it('builds an export URL only when archive families are selected', () => {
    expect(librarySnapshotApi.exportUrl('library-1', [])).toBe(
      '/v1/content/libraries/library-1/snapshot',
    )
    expect(librarySnapshotApi.exportUrl('library-1', ['blobs', 'workspace'])).toBe(
      '/v1/content/libraries/library-1/snapshot?include=blobs%2Cworkspace',
    )
  })

  it('propagates a non-success snapshot import as a structured API error', async () => {
    const file = new NodeFile(['archive'], 'snapshot.tar.zst', {
      type: 'application/zstd',
    }) as unknown as File
    vi.spyOn(globalThis, 'fetch').mockResolvedValue(
      new Response('Archive is incompatible', { status: 422 }),
    )

    await expect(librarySnapshotApi.import('library-1', file, 'reject')).rejects.toMatchObject({
      status: 422,
      body: { detail: 'Archive is incompatible', status: 422 },
    })
  })
})
