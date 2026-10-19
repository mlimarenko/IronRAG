import { describe, expect, it } from 'vitest'

import { createBrowserMockHandlers } from './e2e'
import { catalogLibrary } from './fixtures/catalog-library'
import iamTokens from './fixtures/iam-tokens'
import documentGridItems from './fixtures/document-grid'
import { documentGridFacets, documentGridPage } from './document-grid'
import { server } from './server'

describe('remote library facet browser fixtures', () => {
  it.each(['queue', 'documents', 'calls', '__proto__', 'constructor', 'unknown'])(
    'preserves zero and unavailable values for %s',
    async (column) => {
      const library = catalogLibrary()
      server.use(
        ...createBrowserMockHandlers({
          libraryQueue: {
            catalog: [library],
            items: [],
            libraries: {
              items: [
                {
                  ...library,
                  queuedJobCount: 0,
                  workspace: {
                    id: library.workspaceId,
                    slug: 'workspace',
                    displayName: 'Workspace',
                  },
                  costSummary: null,
                },
              ],
              totalCount: 1,
              facets: { activeCount: 1, readyCount: 1, workspaceCount: 1 },
              aggregateCostSummary: null,
            },
          },
        }),
      )
      const response = await fetch(`/v1/admin/libraries/facets?facetColumn=${column}`)
      expect(response.status).toBe(200)
      await expect(response.json()).resolves.toEqual({
        column,
        values: [column === 'queue' ? { number: 0, count: 1 } : { count: 1 }],
        nextCursor: null,
      })
    },
  )
})

describe('remote document grid browser fixtures', () => {
  const items = documentGridItems()

  it('keeps unpublished metadata null and distinct from a zero-byte publication', async () => {
    server.use(
      ...createBrowserMockHandlers({
        documents: {
          document: { id: 'unpublished', fileName: null, body: '', status: 'empty' },
          revisions: [],
        },
      }),
    )
    const unpublished = await fetch('/v1/content/libraries/library-demo-1/documents')
    const payload = await unpublished.json()
    expect(payload.items[0]).toMatchObject({
      fileName: null,
      fileType: null,
      fileSize: null,
      status: 'empty',
    })
    server.use(
      ...createBrowserMockHandlers({
        documents: {
          document: { id: 'published-empty', fileName: 'empty.txt', fileType: 'txt', body: '' },
          revisions: [],
        },
      }),
    )
    const published = await fetch('/v1/content/libraries/library-demo-1/documents')
    const publishedPayload = await published.json()
    expect(publishedPayload.items[0]).toMatchObject({ fileName: 'empty.txt', fileSize: 0 })
  })

  it('rejects an explicitly empty ID scope at both HTTP reader boundaries', async () => {
    server.use(
      ...createBrowserMockHandlers({
        documents: {
          items,
          document: { id: items[0]!.id, fileName: items[0]!.fileName, body: 'fixture' },
          revisions: [],
        },
      }),
    )
    for (const path of ['documents?ids=', 'documents/facets?facetColumn=file_name&ids=%20%2C%20']) {
      const response = await fetch(`/v1/content/libraries/library-demo-1/${path}`)
      expect(response.status).toBe(400)
    }
  })

  it('pages after full-dataset sorting and searches data beyond the first page', () => {
    const first = documentGridPage(items, new URLSearchParams({ limit: '25' }))
    expect(first.items).toHaveLength(25)
    expect(first.items.some((item) => item.fileName === 'Needle document.pdf')).toBe(false)
    expect(first.nextCursor).toBe('25')
    const second = documentGridPage(items, new URLSearchParams({ limit: '25', cursor: '25' }))
    expect(second.items).toHaveLength(6)
    expect(second.items.some((item) => item.fileName === 'Needle document.pdf')).toBe(true)
    const sorted = documentGridPage(
      items,
      new URLSearchParams({ limit: '25', sortBy: 'cost', sortOrder: 'desc' }),
    )
    expect(sorted.items[0]?.fileName).toBe('Needle document.pdf')
    const searched = documentGridPage(items, new URLSearchParams({ search: '2037' }))
    expect(searched.items.map((item) => item.fileName)).toEqual(['Needle document.pdf'])
  })

  it('honors full scope and self-excludes only the selected facet column', () => {
    const query = new URLSearchParams({
      status: 'ready',
      facetColumn: 'file_type',
      filters: JSON.stringify({ fileType: ['text/plain'] }),
    })
    expect(documentGridPage(items, query).totalCount).toBe(0)
    const facets = documentGridFacets(items, query)
    expect(facets.values).toEqual([{ value: 'application/pdf', count: 16 }])
    const names = documentGridFacets(
      items,
      new URLSearchParams({ facetColumn: 'file_name', facetSearch: 'NEEDLE' }),
    )
    expect(names.values).toEqual([{ value: 'Needle document.pdf', count: 1 }])
  })

  it('keeps decimal prices distinct and orders them numerically without Number rounding', () => {
    const priced = [
      { ...items[0]!, cost: '1.20' },
      { ...items[1]!, cost: '1.11' },
      { ...items[2]!, cost: '9007199254740993.000001' },
      { ...items[3]!, cost: '9007199254740993.000002' },
    ]
    expect(
      documentGridPage(priced, new URLSearchParams({ sortBy: 'cost', sortOrder: 'asc' })).items.map(
        (item) => item.cost,
      ),
    ).toEqual(['1.11', '1.20', '9007199254740993.000001', '9007199254740993.000002'])
  })

  it('applies typed display ranges before paging and to the full facet scope', () => {
    const query = new URLSearchParams({
      search: '1m 30s',
      searchRanges: JSON.stringify({
        pipelineTime: { min: 90_000, max: 91_000 },
      }),
    })
    expect(documentGridPage(items, query).items.map((row) => row.fileName)).toEqual([
      'Needle document.pdf',
    ])
    query.set('facetColumn', 'file_size')
    expect(documentGridFacets(items, query).values).toEqual([{ value: 1537, count: 1 }])
  })
})

describe('token table browser mocks', () => {
  it('returns the full token set and applies only the requested workspace scope', async () => {
    const tokens = iamTokens()
    const other = {
      ...tokens[0]!,
      scope: {
        ...tokens[0]!.scope,
        workspace: { id: 'other-workspace', displayName: 'Other workspace' },
      },
    }
    server.use(...createBrowserMockHandlers({ tokens: [...tokens.slice(1), other] }))
    const all = await fetch('http://localhost:3000/v1/iam/tokens')
    await expect(all.json()).resolves.toHaveLength(31)
    const scoped = await fetch('http://localhost:3000/v1/iam/tokens?workspaceId=other-workspace')
    await expect(scoped.json()).resolves.toEqual([other])
    const empty = await fetch('http://localhost:3000/v1/iam/tokens?workspaceId=missing')
    await expect(empty.json()).resolves.toEqual([])
  })

  it('resolves a detail by principal ID and returns HTTP404 for an unknown token', async () => {
    const tokens = iamTokens()
    server.use(...createBrowserMockHandlers({ tokens }))
    const detail = await fetch('http://localhost:3000/v1/iam/tokens/grid-token-30')
    expect(detail.status).toBe(200)
    await expect(detail.json()).resolves.toMatchObject({
      label: 'Needle token',
      principalId: 'grid-token-30',
    })
    const missing = await fetch('http://localhost:3000/v1/iam/tokens/missing')
    expect(missing.status).toBe(404)
  })
})

describe('assistant session browser mocks', () => {
  it('persists rename and delete mutations in the mocked session list', async () => {
    server.use(
      ...createBrowserMockHandlers({
        authenticated: true,
        querySessions: [
          {
            conversationState: 'active',
            createdAt: '2026-04-10T10:00:00Z',
            id: 'session-1',
            libraryId: 'library-1',
            title: 'Original title',
            turnCount: 2,
            updatedAt: '2026-04-10T10:00:00Z',
            workspaceId: 'workspace-1',
          },
        ],
      }),
    )

    const initialList = await fetch(
      'http://localhost:3000/v1/query/libraries/library-demo-1/sessions',
    )
    await expect(initialList.json()).resolves.toEqual(
      expect.objectContaining({
        items: [expect.objectContaining({ id: 'session-1', title: 'Original title' })],
        total: 1,
      }),
    )

    const renameResponse = await fetch('http://localhost:3000/v1/query/sessions/session-1', {
      body: JSON.stringify({ title: '  Durable   title  ' }),
      headers: { 'content-type': 'application/json' },
      method: 'PATCH',
    })
    expect(renameResponse.status).toBe(200)
    await expect(renameResponse.json()).resolves.toMatchObject({
      id: 'session-1',
      title: 'Durable title',
    })

    const renamedList = await fetch(
      'http://localhost:3000/v1/query/libraries/library-demo-1/sessions',
    )
    await expect(renamedList.json()).resolves.toEqual(
      expect.objectContaining({
        items: [expect.objectContaining({ id: 'session-1', title: 'Durable title' })],
        total: 1,
      }),
    )

    const deleteResponse = await fetch('http://localhost:3000/v1/query/sessions/session-1', {
      method: 'DELETE',
    })
    expect(deleteResponse.status).toBe(204)

    const deletedList = await fetch(
      'http://localhost:3000/v1/query/libraries/library-demo-1/sessions',
    )
    await expect(deletedList.json()).resolves.toEqual(
      expect.objectContaining({ items: [], total: 0 }),
    )
  })
})
