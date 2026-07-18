import { File as NodeFile } from 'node:buffer'

import { afterEach, describe, expect, it, vi } from 'vitest'

import { SnapshotImportTimeoutError, documentsApi, librarySnapshotApi } from './documents'
import { client } from './generated/client.gen'

describe('documentsApi', () => {
  afterEach(() => {
    client.setConfig({ baseUrl: '' })
    vi.restoreAllMocks()
  })

  it('patches document hints with cookie session auth', async () => {
    const requests: Array<{ input: RequestInfo | URL; init?: RequestInit | undefined }> = []
    const fetchMock = vi
      .spyOn(globalThis, 'fetch')
      .mockImplementation(async (input: RequestInfo | URL, init?: RequestInit) => {
        requests.push({ input, init })
        return new Response(null, { status: 204 })
      })

    const result = await documentsApi.updateDocumentHint('doc-1', null)

    expect(fetchMock).toHaveBeenCalledTimes(1)
    expect(result).toBeNull()
    expect(requests).toHaveLength(1)
    const request = requests[0]
    if (!request) throw new Error('expected a captured fetch request')
    expect(String(request.input)).toBe('/v1/content/documents/doc-1')
    expect(request.init?.method).toBe('PATCH')
    expect(request.init?.credentials).toBe('include')
    expect(request.init?.headers).toEqual({ 'Content-Type': 'application/json' })
    expect(request.init?.body).toBe(JSON.stringify({ documentHint: null }))
  })

  it('returns the normalized hint from the patch response', async () => {
    vi.spyOn(globalThis, 'fetch').mockResolvedValue(
      new Response(JSON.stringify({ activeRevision: { documentHint: 'Server hint' } }), {
        headers: { 'Content-Type': 'application/json' },
        status: 200,
      }),
    )

    await expect(documentsApi.updateDocumentHint('doc-1', ' Client hint ')).resolves.toBe(
      'Server hint',
    )
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
      kind: 'accepted',
      operation: {
        operationId: '019e37a5-6295-7022-b0ec-cdc0bcd03716',
        workspaceId: '019e37a5-6295-7022-b0ec-cdc0bcd03717',
        libraryId: '019e37a5-6295-7022-b0ec-cdc0bcd03715',
        overwriteMode: 'replace',
        archiveBytes: payload.length,
      },
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
})
