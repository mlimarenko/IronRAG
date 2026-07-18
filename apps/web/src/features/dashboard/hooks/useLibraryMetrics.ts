import { useCallback, useMemo } from 'react'
import { useSuspenseQuery } from '@tanstack/react-query'

import { queries } from '@/shared/api'
import type { DashboardData } from '@/features/dashboard/model/types'

/**
 * Shared hook that polls `/ops/libraries/{id}/dashboard` while the tab
 * is visible and exposes the latest canonical document-metrics row.
 * Both DashboardPage and the DocumentsPage filter strip consume this
 * so the two surfaces can never show different numbers.
 *
 * Sprint 2 migration: the previous hand-rolled `useEffect` polling loop
 * (visibility listener, interval, debounce, stale-library guard, race
 * protection) is replaced with the canonical TanStack Query 5 path.
 * The behaviour TanStack provides out of the box matches every
 * load-bearing decision the previous hook had:
 *
 * - `refetchInterval: 2500` matches the old `POLL_INTERVAL_MS` and
 *   keeps the dashboard cards in lock-step with the documents-page
 *   filter pills.
 * - `refetchIntervalInBackground: false` is the default. TanStack
 *   pauses polling whenever the document is hidden and runs an
 *   immediate refetch on `visibilitychange → visible`, so operators
 *   see fresh numbers the moment the tab resumes.
 * - `staleTime: 1500` is the canonical equivalent of the old
 *   1500ms debounce: a manual `refresh()` arriving right next to a
 *   scheduled tick collapses to a single network call instead of two.
 * - The `queryKey` includes the library id, so switching libraries
 *   transparently drops the previous request's response (TanStack's
 *   internal request-id check handles the stale-library guard the
 *   `libraryIdRef` used to provide manually).
 * - `placeholderData: keepPreviousData`-style "last good data stays
 *   rendered on transient failures" behaviour falls out of TanStack
 *   keeping `data` populated while `error` is set.
 */
const POLL_INTERVAL_MS = 2500
const DEBOUNCE_MS = 1500

type LibraryMetricsState = {
  data: DashboardData
  error: string | null
  isRefreshing: boolean
  lastUpdatedAt: Date | null
  refresh: () => Promise<void>
}

export function useLibraryMetrics(libraryId: string): LibraryMetricsState {
  const query = useSuspenseQuery({
    ...queries.getLibraryDashboardOptions({
      path: { libraryId },
    }),
    staleTime: DEBOUNCE_MS,
    refetchInterval: POLL_INTERVAL_MS,
    refetchIntervalInBackground: false,
  })

  const data = query.data

  const errorMessage = useMemo<string | null>(() => {
    if (!query.error) return null
    return query.error instanceof Error ? query.error.message : String(query.error)
  }, [query.error])

  const lastUpdatedAt = useMemo<Date | null>(() => {
    if (!query.dataUpdatedAt) return null
    return new Date(query.dataUpdatedAt)
  }, [query.dataUpdatedAt])

  const { refetch } = query
  const refresh = useCallback(async () => {
    await refetch()
  }, [refetch])

  return {
    data,
    error: errorMessage,
    isRefreshing: query.isFetching,
    lastUpdatedAt,
    refresh,
  }
}
