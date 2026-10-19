import {
  keepPreviousData,
  useInfiniteQuery,
  useMutation,
  useQuery,
  useQueryClient,
  type Query,
} from '@tanstack/react-query'
import type { TFunction } from 'i18next'
import { useCallback, useEffect, useMemo, useState } from 'react'
import {
  ASYNC_OPERATION_TERMINAL_STATES,
  adminLibraryFacetsInfiniteOptions,
  adminLibraryListQuery,
  Catalog,
  librarySnapshotApi,
  queries,
  unwrap,
} from '@/shared/api'
import type {
  AdminLibraryColumnFilters,
  AdminLibrarySearchHints,
  AdminLibrarySortKey,
  AdminLibrarySortOrder,
  AsyncOperationDetailResponse,
  CatalogLibraryResponse,
} from '@/shared/api/generated'
import type { GridFacetPage } from '@/shared/data-grid'
import { buildGridServerFacets } from '@/shared/data-grid'
import type { Locale } from '@/shared/types'
import { ADMIN_LIBRARY_FACETS_QUERY_KEY } from '@/shared/api/libraries'
import {
  adaptAdminLibrariesPage,
  buildWebIngestFilter,
  type LibrarySettingsDraft,
} from '@/features/admin/libraries/model/librariesAdapter'
import {
  libraryFacetOptions,
  type LibraryFacetRow,
  type LibraryGridColumnId,
} from '@/features/admin/libraries/model/librariesGrid'
import { deriveLibraryFacetSearch } from '@/features/admin/libraries/model/librariesSearch'

const OPERATION_POLL_INTERVAL_MS = 2_000
const OPERATION_POLL_LIMIT = 900

/** Debounce a value so a keystroke does not fire a request per character. */
export function useDebouncedValue<T>(value: T, delayMs: number): T {
  const [debounced, setDebounced] = useState(value)
  useEffect(() => {
    const timeout = window.setTimeout(() => setDebounced(value), delayMs)
    return () => window.clearTimeout(timeout)
  }, [value, delayMs])
  return debounced
}

export type AdminLibrariesFilters = {
  search: string
  normalizedSearch: string | null
  hints: AdminLibrarySearchHints | null
  filters: AdminLibraryColumnFilters | undefined
  sortBy: AdminLibrarySortKey
  sortOrder: AdminLibrarySortOrder
  offset: number
  limit: number
}

export type RestoreSnapshotInput = {
  scope: 'library' | 'workspace'
  libraryId: string
  workspaceId: string
  file: File
  overwrite: 'reject' | 'replace'
}

export type SaveLibrarySettingsInput = {
  libraryId: string
  draft: LibrarySettingsDraft
  current: CatalogLibraryResponse
}

export type DeleteLibraryInput = {
  libraryId: string
  workspaceId: string
}

function generatedOperationId(query: Query): string | null {
  const root = query.queryKey[0]
  if (!root || typeof root !== 'object' || !('_id' in root)) return null
  return typeof root._id === 'string' ? root._id : null
}

function sleep(ms: number) {
  return new Promise<void>((resolve) => window.setTimeout(resolve, ms))
}

function assertSuccessfulOperation(operation: AsyncOperationDetailResponse): void {
  if (operation.status === 'ready') return
  throw new Error(operation.failureCode ?? `Operation finished with status ${operation.status}`)
}

export function useAdminLibrariesQuery(filters: AdminLibrariesFilters) {
  return useQuery({
    ...queries.listAdminLibrariesOptions({
      query: adminLibraryListQuery({
        ...(filters.search ? { search: filters.search } : {}),
        ...(filters.normalizedSearch ? { normalizedSearch: filters.normalizedSearch } : {}),
        ...(filters.hints ? { searchHints: filters.hints } : {}),
        ...(filters.filters ? { filters: filters.filters } : {}),
        sortBy: filters.sortBy,
        sortOrder: filters.sortOrder,
        offset: filters.offset,
        limit: Math.min(filters.limit, 100),
      }),
    }),
    placeholderData: keepPreviousData,
    select: adaptAdminLibrariesPage,
  })
}

export interface LibraryFacetScope {
  search: string
  normalizedSearch: string | null
  hints: AdminLibrarySearchHints | null
  filters: AdminLibraryColumnFilters | undefined
}

/**
 * The open menu owns UI state only; the complete authorized scope is the query
 * cache key. One infinite query at a time — the column whose menu is open —
 * self-excluding that column at the server.
 */
export function useLibraryFacets({
  enabled,
  locale,
  scope,
  t,
}: Readonly<{ enabled: boolean; locale: Locale; scope: LibraryFacetScope; t: TFunction }>) {
  const [activeColumn, setActiveColumn] = useState<LibraryGridColumnId | null>(null)
  const [search, setSearch] = useState('')
  const debouncedSearch = useDebouncedValue(search, 300)
  const facetSearch = search ? debouncedSearch : ''
  // The menu's own term derives typed hints scoped to the open column, never a
  // copy of the whole-table search hints. The query key carries them, so a
  // column or term change fetches a distinct, bounded server facet page.
  const facetSearchHints = useMemo(
    () =>
      activeColumn === null ? null : deriveLibraryFacetSearch(activeColumn, facetSearch, locale, t),
    [activeColumn, facetSearch, locale, t],
  )
  const query = useInfiniteQuery({
    ...adminLibraryFacetsInfiniteOptions({
      column: activeColumn ?? 'library',
      facetLimit: 50,
      ...(scope.search ? { search: scope.search } : {}),
      ...(scope.normalizedSearch ? { normalizedSearch: scope.normalizedSearch } : {}),
      ...(scope.hints ? { searchHints: scope.hints } : {}),
      ...(scope.filters ? { filters: scope.filters } : {}),
      ...(facetSearch ? { facetSearch } : {}),
      ...(facetSearchHints ? { facetSearchHints } : {}),
    }),
    enabled: enabled && activeColumn !== null,
    staleTime: 0,
  })
  const values = useMemo(() => query.data?.pages.flatMap((page) => page.values) ?? [], [query.data])
  const facetRows = useMemo<LibraryFacetRow[]>(
    () =>
      values.map((value) => {
        if (activeColumn !== 'cost')
          return {
            count: value.count,
            value: value.text ?? value.number ?? value.flag ?? null,
          }
        return {
          count: value.count,
          decimal: value.decimal ?? null,
          currencyCode: value.currencyCode ?? null,
          coverage: value.coverage ?? null,
          unpricedCallCount: value.unpricedCallCount ?? null,
        }
      }),
    [activeColumn, values],
  )
  const page = useCallback(
    (columnId: string): GridFacetPage | undefined => {
      if (columnId !== activeColumn) return undefined
      return {
        options: libraryFacetOptions(activeColumn, facetRows, t, locale),
        hasMore: query.hasNextPage,
        pending: query.isFetching || facetSearch !== search,
        error: query.isError,
      }
    },
    [
      activeColumn,
      facetRows,
      facetSearch,
      locale,
      query.hasNextPage,
      query.isError,
      query.isFetching,
      search,
      t,
    ],
  )
  const serverFacets = buildGridServerFacets({
    page,
    setActiveColumn: (columnId) => setActiveColumn(columnId as LibraryGridColumnId),
    clearSearch: () => setSearch(''),
    setSearch,
    hasNextPage: query.hasNextPage,
    isFetching: query.isFetching,
    fetchNextPage: () => void query.fetchNextPage(),
  })
  return { serverFacets }
}

export function useLibraryInspectorQueries(libraryId: string | null, enabled: boolean) {
  const safeLibraryId = libraryId ?? ''
  const library = useQuery({
    ...queries.getCatalogLibraryOptions({ path: { libraryId: safeLibraryId } }),
    enabled: enabled && libraryId !== null,
  })
  const retrieval = useQuery({
    ...queries.getCatalogLibraryRetrievalConfigOptions({ path: { libraryId: safeLibraryId } }),
    enabled: enabled && libraryId !== null,
  })
  const state = useQuery({
    ...queries.getLibraryStateOptions({ path: { libraryId: safeLibraryId } }),
    enabled: enabled && libraryId !== null,
    staleTime: 15_000,
  })
  const queue = useQuery({
    ...queries.listIngestQueueOptions({ query: { libraryId: safeLibraryId } }),
    enabled: enabled && libraryId !== null,
    staleTime: 15_000,
  })
  const completedJobs = useQuery({
    ...queries.listIngestJobsOptions({
      path: { libraryId: safeLibraryId },
      query: { limit: 100, status: 'completed' },
    }),
    enabled: enabled && libraryId !== null,
    staleTime: 30_000,
  })
  const failedJobs = useQuery({
    ...queries.listIngestJobsOptions({
      path: { libraryId: safeLibraryId },
      query: { limit: 100, status: 'failed' },
    }),
    enabled: enabled && libraryId !== null,
    staleTime: 30_000,
  })

  return { library, retrieval, state, queue, completedJobs, failedJobs }
}

export function useLibraryAdminMutations() {
  const queryClient = useQueryClient()

  const invalidateLibraryQueries = useCallback(async () => {
    await Promise.all([
      queryClient.invalidateQueries({ queryKey: ADMIN_LIBRARY_FACETS_QUERY_KEY }),
      queryClient.invalidateQueries({
        predicate: (query) => {
          const operationId = generatedOperationId(query)
          return operationId === 'listAdminLibraries' || operationId === 'getCatalogLibrary'
        },
      }),
    ])
  }, [queryClient])

  const waitForOperation = useCallback(
    async (operationId: string): Promise<AsyncOperationDetailResponse> => {
      for (let attempt = 0; attempt < OPERATION_POLL_LIMIT; attempt += 1) {
        const operation = await queryClient.fetchQuery({
          ...queries.getAsyncOperationOptions({ path: { operationId } }),
          staleTime: 0,
        })
        if (ASYNC_OPERATION_TERMINAL_STATES.has(operation.status)) return operation
        await sleep(OPERATION_POLL_INTERVAL_MS)
      }
      throw new Error('Operation did not finish before the polling deadline')
    },
    [queryClient],
  )

  const saveSettings = useMutation({
    mutationFn: async ({ libraryId, draft, current }: SaveLibrarySettingsInput) => {
      const crawlFilter = buildWebIngestFilter(
        draft.crawlAllowPatterns,
        draft.crawlBlockPatterns,
        current.webIngestPolicy.crawlFilter,
      )
      const materializationFilter = buildWebIngestFilter(
        draft.materializationAllowPatterns,
        draft.materializationBlockPatterns,
        current.webIngestPolicy.materializationFilter,
      )

      // Refetch only after every write settles, including late successes after
      // an early failure; otherwise onSettled can cache the pre-write facets.
      const outcomes = await Promise.allSettled([
        Catalog.updateCatalogLibrary({
          path: { libraryId },
          body: {
            displayName: draft.displayName.trim(),
            slug: draft.slug.trim(),
            description: draft.description.trim() || null,
            lifecycleState: draft.lifecycleState,
            extractionPrompt: draft.extractionPrompt.trim() || null,
            includeDocumentHintInMcpAnswers: draft.includeDocumentHintInMcpAnswers,
          },
        }).then((result) => unwrap(result)),
        Catalog.updateCatalogLibraryRecognitionPolicy({
          path: { libraryId },
          body: { rasterImageEngine: draft.recognitionEngine },
        }).then((result) => unwrap(result)),
        Catalog.updateCatalogLibraryRetrievalConfig({
          path: { libraryId },
          body: { lexical: { textSearchConfig: draft.textSearchConfig.trim() } },
        }).then((result) => unwrap(result)),
        Catalog.updateCatalogLibraryWebIngestPolicy({
          path: { libraryId },
          body: { crawlFilter, materializationFilter },
        }).then((result) => unwrap(result)),
      ])
      const failure = outcomes.find((outcome) => outcome.status === 'rejected')
      if (failure) throw failure.reason
    },
    onSettled: invalidateLibraryQueries,
  })

  const deleteLibrary = useMutation({
    mutationFn: async ({ libraryId, workspaceId }: DeleteLibraryInput) => {
      const admission = unwrap(
        await Catalog.deleteCatalogLibrary({ path: { workspaceId, libraryId } }),
      )
      const operation = await waitForOperation(admission.operationId)
      assertSuccessfulOperation(operation)
    },
    onSettled: invalidateLibraryQueries,
  })

  const restoreSnapshot = useMutation({
    mutationFn: async ({
      scope,
      libraryId,
      workspaceId,
      file,
      overwrite,
    }: RestoreSnapshotInput) => {
      const admission =
        scope === 'library'
          ? await librarySnapshotApi.import(libraryId, file, overwrite)
          : unwrap(
              await Catalog.importCatalogWorkspaceSnapshot({
                path: { workspaceId },
                query: { onConflict: overwrite },
                body: file,
              }),
            )
      const operation = await waitForOperation(admission.operationId)
      assertSuccessfulOperation(operation)
    },
    onSettled: invalidateLibraryQueries,
  })

  const loadLibrary = useCallback(
    (libraryId: string) =>
      queryClient.fetchQuery(queries.getCatalogLibraryOptions({ path: { libraryId } })),
    [queryClient],
  )

  return { saveSettings, deleteLibrary, restoreSnapshot, loadLibrary }
}

type SnapshotIncludeKind = 'library_data' | 'blobs' | 'workspace' | 'ai_config'

export function downloadLibrarySnapshot(libraryId: string, include: SnapshotIncludeKind[]): void {
  librarySnapshotApi.downloadExport(libraryId, include)
}

export function downloadWorkspaceSnapshot(
  workspaceId: string,
  include: SnapshotIncludeKind[],
): void {
  const query = new URLSearchParams({ include: include.join(',') })
  const anchor = document.createElement('a')
  anchor.href = `/v1/catalog/workspaces/${encodeURIComponent(workspaceId)}/snapshot?${query}`
  anchor.rel = 'noopener'
  document.body.appendChild(anchor)
  anchor.click()
  anchor.remove()
}
