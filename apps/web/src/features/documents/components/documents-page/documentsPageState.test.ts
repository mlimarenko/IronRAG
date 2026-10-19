import { readdirSync, readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { act, renderHook } from '@testing-library/react'
import type { ReactNode } from 'react'
import { createElement } from 'react'
import { MemoryRouter, useNavigate } from 'react-router-dom'
import { describe, expect, it, vi } from 'vitest'

import {
  documentsEmptyMessageKey,
  hasRecordedSpend,
  isLibraryEmpty,
  parseGridFilters,
  selectAllMatchingAvailable,
  serializeGridFilters,
  useDocumentsPageUrlState,
} from './documentsPageState'
import { gridFacetKey } from '@/shared/data-grid'
import { reduceGridState } from '@/shared/data-grid/model'

function urlStateAt(search: string, libraryId: string | null = 'library-one') {
  function Wrapper({ children }: Readonly<{ children: ReactNode }>) {
    return createElement(MemoryRouter, { initialEntries: [`/documents${search}`] }, children)
  }
  return renderHook(
    ({ libraryId }) => ({
      ...useDocumentsPageUrlState({
        libraryId,
        tableState: {
          pageSize: 50,
          sort: { key: 'uploaded_at', direction: 'desc' },
          showDetailColumns: false,
        },
        setTableState: vi.fn(),
      }),
      navigate: useNavigate(),
    }),
    { wrapper: Wrapper, initialProps: { libraryId } },
  )
}

describe('document status filter vocabulary', () => {
  it('resolves the in-flight union to both of the statuses it stands for', () => {
    const state = urlStateAt('?status=inflight').result.current

    expect(state.statusBucket).toBe('inflight')
    expect([...(state.listFilter.status ?? [])].sort()).toEqual(['processing', 'queued'])
  })

  it('resolves the not-ready union to everything that is not available yet', () => {
    const state = urlStateAt('?status=notready').result.current

    expect(state.statusBucket).toBe('notready')
    expect([...(state.listFilter.status ?? [])].sort()).toEqual([
      'canceled',
      'empty',
      'failed',
      'processing',
      'queued',
    ])
  })

  it.each([
    ['vectorgap', 'vector_gap'],
    ['graphgap', 'graph_gap'],
    ['graphready', 'graph_ready'],
  ] as const)(
    'maps %s to the backend %s indexing invariant without status narrowing',
    (bucket, indexing) => {
      const state = urlStateAt(`?status=${bucket}`).result.current

      expect(state.statusBucket).toBe(bucket)
      // Indexing buckets are not lifecycle statuses: narrowing to ready would
      // hide incomplete documents that violate the selected invariant.
      expect(state.listFilter.status).toBeUndefined()
      expect(state.listFilter.indexing).toBe(indexing)
    },
  )

  it('shows everything for a status it cannot parse', () => {
    // The fallback itself is fine — it is why an unparsed link never looks
    // broken, and why the links that named a comma-joined status list went
    // unnoticed for so long while quietly showing all documents.
    const state = urlStateAt('?status=processing,queued').result.current

    expect(state.statusBucket).toBe('all')
    expect(state.listFilter).toEqual({})
  })
})

describe('duration snapshot scope', () => {
  const snapshot = '2026-10-01T12:00:00Z'

  it('drops the previous library clock and inspector without dropping grid filters', () => {
    const filters = { file_type: [gridFacetKey('application/pdf')] }
    const { result, rerender } = urlStateAt(
      `?q=needle&snapshotAt=${snapshot}&documentId=old-document&filters=${encodeURIComponent(serializeGridFilters(filters)!)}`,
    )
    rerender({ libraryId: 'library-two' })
    expect(result.current.snapshotAt).toBeNull()
    expect(result.current.selectedDocumentId).toBeNull()
    expect(result.current.searchQuery).toBe('needle')
    expect(result.current.filters).toEqual(filters)
  })

  it('keeps a duration-value clock, but not the inspector, when switching libraries', () => {
    const filters = { pipeline_time: [gridFacetKey(90_125)] }
    const { result, rerender } = urlStateAt(
      `?snapshotAt=${snapshot}&documentId=old-document&filters=${encodeURIComponent(serializeGridFilters(filters)!)}`,
    )
    rerender({ libraryId: 'library-two' })
    expect(result.current.snapshotAt).toBe(snapshot)
    expect(result.current.selectedDocumentId).toBeNull()
    expect(result.current.filters).toEqual(filters)
  })

  it('preserves a reload clock while the initial library scope hydrates', () => {
    const { result, rerender } = urlStateAt(`?snapshotAt=${snapshot}`, null)
    rerender({ libraryId: 'library-one' })
    expect(result.current.snapshotAt).toBe(snapshot)
  })

  it('remembers the previous library across a transient empty scope', () => {
    const { result, rerender } = urlStateAt(`?snapshotAt=${snapshot}`)
    rerender({ libraryId: null })
    rerender({ libraryId: 'library-two' })
    expect(result.current.snapshotAt).toBeNull()
  })

  it('ignores a delayed URL patch belonging to the previous library', () => {
    const { result, rerender } = urlStateAt(`?snapshotAt=${snapshot}`)
    const previousScopePatch = result.current.updateSearchParamState
    rerender({ libraryId: 'library-two' })
    act(() => previousScopePatch({ snapshotAt: snapshot, documentId: 'old-document' }))
    expect(result.current.snapshotAt).toBeNull()
    expect(result.current.selectedDocumentId).toBeNull()
  })

  it('does not revive an old callback when switching back to its library', () => {
    const { result, rerender } = urlStateAt(`?snapshotAt=${snapshot}`)
    const previousVisitPatch = result.current.updateSearchParamState
    rerender({ libraryId: 'library-two' })
    rerender({ libraryId: 'library-one' })
    act(() => previousVisitPatch({ snapshotAt: snapshot, documentId: 'old-document' }))
    expect(result.current.snapshotAt).toBeNull()
    expect(result.current.selectedDocumentId).toBeNull()
  })

  it('merges same-turn snapshot and inspector updates without losing either URL field', () => {
    const { result } = urlStateAt('?q=needle')
    act(() => {
      result.current.updateSearchParamState({ snapshotAt: snapshot })
      result.current.updateSearchParamState({ documentId: 'selected' })
    })
    expect(result.current.snapshotAt).toBe(snapshot)
    expect(result.current.selectedDocumentId).toBe('selected')
    expect(result.current.searchQuery).toBe('needle')
  })
  it('reduces same-turn facet selections against the pending URL and preserves their reload state', () => {
    const { result } = urlStateAt('?q=needle&documentId=selected')
    act(() => {
      for (const value of ['application/pdf', 'text/plain']) {
        result.current.applyGridState((current) =>
          reduceGridState(current, {
            type: 'toggleFilter',
            columnId: 'file_type',
            key: gridFacetKey(value),
          }),
        )
      }
    })
    const expected = { file_type: [gridFacetKey('application/pdf'), gridFacetKey('text/plain')] }
    expect(result.current.filters).toEqual(expected)
    expect(result.current.columnFilters?.fileType).toEqual(['application/pdf', 'text/plain'])
    expect(result.current.searchQuery).toBe('needle')
    expect(result.current.selectedDocumentId).toBeNull()
    const reload = urlStateAt(`?filters=${encodeURIComponent(serializeGridFilters(expected)!)}`)
    expect(reload.result.current.filters).toEqual(expected)
  })

  it('resyncs pending patches to a navigation before the next inspector update', () => {
    const { result } = urlStateAt(`?q=old&snapshotAt=${snapshot}`)
    act(() => result.current.updateSearchParamState({ documentId: 'old-selection' }))
    act(() => {
      void result.current.navigate('/documents?q=new&status=failed')
    })
    act(() => result.current.updateSearchParamState({ documentId: 'new-selection' }))
    expect(result.current.searchQuery).toBe('new')
    expect(result.current.statusBucket).toBe('failed')
    expect(result.current.snapshotAt).toBeNull()
    expect(result.current.selectedDocumentId).toBe('new-selection')
  })

  it('keeps the pinned snapshot on a reload of the unchanged scope', () => {
    expect(urlStateAt(`?snapshotAt=${snapshot}`).result.current.snapshotAt).toBe(snapshot)
  })

  it('keeps a duration filter on the clock of the selected facet value across scope changes', () => {
    const { result } = urlStateAt(`?snapshotAt=${snapshot}`)
    const filters = { pipeline_time: [gridFacetKey(90_125)] }
    act(() => result.current.applyGridState({ ...result.current.gridState, filters }))
    expect(result.current.snapshotAt).toBe(snapshot)
    act(() => result.current.applyGridState({ ...result.current.gridState, search: 'new search' }))
    expect(result.current.snapshotAt).toBe(snapshot)
    act(() => result.current.applyGridState({ ...result.current.gridState, filters: {} }))
    expect(result.current.snapshotAt).toBeNull()
  })

  it.each([
    { search: 'new term' },
    { sort: { columnId: 'status', direction: 'asc' as const } },
    { filters: { file_type: [gridFacetKey('application/pdf')] } },
    { pageSize: 25 },
  ])('drops the old pin and document selection when changing %j', (change) => {
    const { result } = urlStateAt(`?snapshotAt=${snapshot}&documentId=old-document`)
    act(() => result.current.applyGridState({ ...result.current.gridState, ...change }))
    expect(result.current.snapshotAt).toBeNull()
    expect(result.current.selectedDocumentId).toBeNull()
  })
})

const SOURCE_ROOTS = [
  { dir: resolve(process.cwd(), 'src'), extensions: ['.ts', '.tsx'] },
  { dir: resolve(process.cwd(), '../api/src'), extensions: ['.rs'] },
]

// Tests are excluded on purpose: a test may quote a link precisely because it
// is the one the screen cannot parse — this file does exactly that above.
const isTestFile = (name: string) =>
  name.includes('.test.') || name === 'tests.rs' || name.endsWith('_tests.rs')

function sourceFiles(dir: string, extensions: readonly string[]): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const path = resolve(dir, entry.name)
    if (entry.isDirectory()) return entry.name === 'tests' ? [] : sourceFiles(path, extensions)
    if (isTestFile(entry.name)) return []
    return extensions.some((extension) => entry.name.endsWith(extension)) ? [path] : []
  })
}

describe('links into the filtered document list', () => {
  it('names only status filters this screen can parse', () => {
    // Both halves of the product write these links by hand — the dashboard in
    // TSX, the operator notices in Rust — and an unparsed one costs nothing at
    // the call site: the screen falls back to showing every document. So the
    // check has to run over the sources that emit the links, not over a list
    // of them copied here, which would drift the same way the links did.
    const found = new Map<string, string>()
    for (const { dir, extensions } of SOURCE_ROOTS) {
      const files = sourceFiles(dir, extensions)
      expect(files.length, `no sources under ${dir}`).toBeGreaterThan(0)
      for (const file of files) {
        for (const match of readFileSync(file, 'utf8').matchAll(
          /\/documents\?status=([A-Za-z0-9_,%-]+)/g,
        )) {
          const status = match[1]
          if (status) found.set(status, file)
        }
      }
    }

    expect(found.size, 'no document status links found at all').toBeGreaterThan(0)
    for (const [status, file] of found) {
      const state = urlStateAt(`?status=${status}`).result.current
      expect(state.statusBucket, `${status} (${file})`).not.toBe('all')
      // Parsing is only half of it: a bucket that parses and asks the backend
      // for nothing shows the whole library just as silently as an unparsed
      // link does. Every link must narrow something.
      expect(Object.keys(state.listFilter), `${status} (${file}) narrows nothing`).not.toHaveLength(
        0,
      )
    }
  })
})

describe('isLibraryEmpty', () => {
  const signals = {
    loading: false,
    loadFailed: false,
    searchQuery: '',
    statusBucket: 'all' as const,
    totalCount: 0,
    loadedCount: 0,
    pendingUploadCount: 0,
  }

  it('is empty only when nothing is loading, failing, filtered or queued', () => {
    expect(isLibraryEmpty(signals)).toBe(true)
  })

  it('is not empty while the first page is still loading', () => {
    expect(isLibraryEmpty({ ...signals, loading: true })).toBe(false)
  })

  it('is not empty when the load failed — that is an error, not an empty library', () => {
    expect(isLibraryEmpty({ ...signals, loadFailed: true })).toBe(false)
  })

  it('is not empty when a search or a status bucket narrowed the list to zero', () => {
    expect(isLibraryEmpty({ ...signals, searchQuery: 'alpha' })).toBe(false)
    expect(isLibraryEmpty({ ...signals, statusBucket: 'failed' })).toBe(false)
  })

  it('is not empty while an upload is still queued', () => {
    expect(isLibraryEmpty({ ...signals, pendingUploadCount: 1 })).toBe(false)
  })

  it('is not empty when rows exist, even before the server total arrives', () => {
    expect(isLibraryEmpty({ ...signals, totalCount: null, loadedCount: 3 })).toBe(false)
    expect(isLibraryEmpty({ ...signals, totalCount: 12 })).toBe(false)
  })
})

describe('the active filter empty text', () => {
  it('distinguishes an indexing gap from a true empty library', () => {
    expect(documentsEmptyMessageKey({ searchQuery: '', statusBucket: 'all' })).toBe(
      'documents.noDocs',
    )
    expect(documentsEmptyMessageKey({ searchQuery: 'alpha', statusBucket: 'all' })).toBe(
      'documents.noMatchingDocs',
    )
    expect(documentsEmptyMessageKey({ searchQuery: '', statusBucket: 'failed' })).toBe(
      'documents.noMatchingFilter',
    )
    expect(documentsEmptyMessageKey({ searchQuery: '', statusBucket: 'vectorgap' })).toBe(
      'documents.noVectorGaps',
    )
    expect(documentsEmptyMessageKey({ searchQuery: '', statusBucket: 'graphgap' })).toBe(
      'documents.noGraphGaps',
    )
  })
})

describe('grid filter URL round trip', () => {
  it('keeps the null bucket, a comma inside a name and an exact decimal cost', () => {
    const filters = {
      file_name: [gridFacetKey('a, b.txt')],
      file_type: [gridFacetKey(null)],
      cost: [gridFacetKey('0.123456789012345678')],
      file_size: [gridFacetKey(1024)],
    }
    const encoded = serializeGridFilters(filters)
    expect(encoded).not.toBeNull()
    expect(parseGridFilters(encoded)).toEqual(filters)
    // The decimal cost is transported as its exact source text, not a rounded
    // number: a float would lose the tail of this value.
    expect(encoded).toContain('0.123456789012345678')
  })

  it('drops an unparseable filter rather than half-applying it', () => {
    expect(parseGridFilters('not json')).toEqual({})
    expect(parseGridFilters('{"unknown":["string:x"]}')).toEqual({})
    expect(parseGridFilters('{"status":["not-a-key"]}')).toEqual({})
    expect(parseGridFilters(null)).toEqual({})
  })
})

describe('selectAllMatchingAvailable', () => {
  const base = {
    allVisibleSelected: true,
    filteredTotal: 100,
    hasMorePages: false,
    itemCount: 50,
    selectedCount: 50,
  }

  it('needs a fully selected page and more rows than the page holds', () => {
    expect(selectAllMatchingAvailable(base)).toBe(true)
    expect(selectAllMatchingAvailable({ ...base, allVisibleSelected: false })).toBe(false)
    expect(selectAllMatchingAvailable({ ...base, selectedCount: 100 })).toBe(false)
  })

  it('falls back to the cursor when the filtered total is unknown', () => {
    expect(selectAllMatchingAvailable({ ...base, filteredTotal: null, hasMorePages: true })).toBe(
      true,
    )
    expect(selectAllMatchingAvailable({ ...base, filteredTotal: null, hasMorePages: false })).toBe(
      false,
    )
  })
})

describe('hasRecordedSpend', () => {
  const zero = {
    libraryCost: 0,
    workspaceCost: 0,
    libraryUnpricedCalls: 0,
    workspaceUnpricedCalls: 0,
  }

  it('hides the strip when nothing was charged and nothing went unpriced', () => {
    expect(hasRecordedSpend(zero)).toBe(false)
  })

  it('shows the strip for an unpriced call even when the amount is zero', () => {
    expect(hasRecordedSpend({ ...zero, libraryUnpricedCalls: 1 })).toBe(true)
    expect(hasRecordedSpend({ ...zero, workspaceUnpricedCalls: 1 })).toBe(true)
  })

  it('shows the strip for any recorded amount', () => {
    expect(hasRecordedSpend({ ...zero, libraryCost: 0.01 })).toBe(true)
    expect(hasRecordedSpend({ ...zero, workspaceCost: 2 })).toBe(true)
  })
  it('does not treat a withheld amount or call count as zero spend', () => {
    expect(hasRecordedSpend({ ...zero, libraryCost: null })).toBe(true)
    expect(hasRecordedSpend({ ...zero, workspaceUnpricedCalls: null })).toBe(true)
  })
})
