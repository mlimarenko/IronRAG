import { useCallback, useEffect, useRef, useState } from 'react';

import { dashboardApi } from '@/api';
import type { DashboardData } from '@/pages/dashboard/types';

/**
 * Shared hook that polls `/ops/libraries/{id}/dashboard` while the
 * tab is visible and exposes the latest canonical document-metrics
 * row. Both DashboardPage and the DocumentsPage filter strip consume
 * this so the two surfaces can never show different numbers.
 *
 * Design decisions, all load-bearing — don't tune without re-reading:
 *
 * - `POLL_INTERVAL_MS = 2500` matches `LIST_POLL_INTERVAL_MS` on the
 *   DocumentsPage. Same cadence keeps pills and cards in visual
 *   lock-step when operators switch between the two pages, and the
 *   dashboard handler is cheap (~40 ms p50) compared to the 2.5 s
 *   budget.
 * - The poll pauses while `document.visibilityState === 'hidden'`
 *   and fires one immediate refresh on `visibilitychange` when the
 *   tab becomes visible again, so operators see fresh numbers the
 *   moment they come back to the tab instead of waiting up to
 *   2.5 s.
 * - A tiny `debounceMs = 1500` floor between successive requests
 *   collapses accidental double-refreshes (e.g. a manual button
 *   press landing right before a scheduled tick) without making
 *   the UI feel laggy.
 * - The hook never raises on a transient fetch failure — the last
 *   good `data` stays rendered and the error is surfaced via
 *   `error`. This matches the old one-shot behaviour where an API
 *   blip wiped the card contents; we explicitly preserve the card.
 *
 * The hook is hook-stable by `libraryId`: changing the library
 * triggers a fresh initial fetch and resets the poll loop, so
 * switching between workspaces does not leak stale numbers.
 */
const POLL_INTERVAL_MS = 2500;
const DEBOUNCE_MS = 1500;

export type LibraryMetricsState = {
  data: DashboardData | null;
  error: string | null;
  isInitialLoading: boolean;
  isRefreshing: boolean;
  lastUpdatedAt: Date | null;
  refresh: () => Promise<void>;
};

export function useLibraryMetrics(
  libraryId: string | null | undefined,
): LibraryMetricsState {
  const [data, setData] = useState<DashboardData | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [isInitialLoading, setInitialLoading] = useState<boolean>(
    libraryId ? true : false,
  );
  const [isRefreshing, setRefreshing] = useState<boolean>(false);
  const [lastUpdatedAt, setLastUpdatedAt] = useState<Date | null>(null);

  // Refs that survive re-renders without triggering them. We guard
  // against stale closures (library switches while a fetch is in
  // flight) and against overlapping fetches (successive ticks
  // landing before the previous response returned).
  const libraryIdRef = useRef(libraryId ?? null);
  const isFetchingRef = useRef(false);
  const lastFetchAtRef = useRef<number>(0);

  useEffect(() => {
    libraryIdRef.current = libraryId ?? null;
  }, [libraryId]);

  const fetchOnce = useCallback(
    async (mode: 'initial' | 'refresh' | 'tick') => {
      const currentId = libraryIdRef.current;
      if (!currentId) {
        return;
      }
      if (isFetchingRef.current) {
        return;
      }
      const now = Date.now();
      // Only automatic ticks (interval + visibility-resume) are
      // debounced — manual `refresh` and `initial` are user-initiated
      // and must fire immediately so the operator sees fresh numbers
      // on click / library switch.
      if (mode === 'tick' && now - lastFetchAtRef.current < DEBOUNCE_MS) {
        return;
      }
      isFetchingRef.current = true;
      lastFetchAtRef.current = now;
      if (mode === 'initial') {
        setInitialLoading(true);
      } else {
        setRefreshing(true);
      }
      try {
        const result = (await dashboardApi.getLibraryDashboard(
          currentId,
        )) as DashboardData;
        // Library switched while we were in flight — drop the result
        // so the stale library's numbers never land in state.
        if (libraryIdRef.current !== currentId) {
          return;
        }
        setData(result);
        setError(null);
        setLastUpdatedAt(new Date());
      } catch (err: unknown) {
        if (libraryIdRef.current !== currentId) {
          return;
        }
        const message =
          err instanceof Error ? err.message : 'Failed to load metrics';
        setError(message);
      } finally {
        if (libraryIdRef.current === currentId) {
          setInitialLoading(false);
          setRefreshing(false);
        }
        isFetchingRef.current = false;
      }
    },
    [],
  );

  // Initial fetch + library switch: drop the old data, reset state,
  // fire a fresh initial load.
  useEffect(() => {
    if (!libraryId) {
      setData(null);
      setError(null);
      setInitialLoading(false);
      return;
    }
    setData(null);
    setError(null);
    setInitialLoading(true);
    void fetchOnce('initial');
  }, [libraryId, fetchOnce]);

  // Interval poll + visibility-aware pause/resume.
  useEffect(() => {
    if (!libraryId) {
      return;
    }
    let intervalId: number | undefined;

    const startPolling = () => {
      if (intervalId !== undefined) return;
      intervalId = window.setInterval(() => {
        if (document.visibilityState !== 'visible') return;
        void fetchOnce('tick');
      }, POLL_INTERVAL_MS);
    };
    const stopPolling = () => {
      if (intervalId !== undefined) {
        window.clearInterval(intervalId);
        intervalId = undefined;
      }
    };
    const onVisibilityChange = () => {
      if (document.visibilityState === 'visible') {
        // Immediate one-shot refresh on tab resume so the first
        // number the operator sees is fresh, not up-to-2.5-s stale.
        void fetchOnce('tick');
        startPolling();
      } else {
        stopPolling();
      }
    };

    if (document.visibilityState === 'visible') {
      startPolling();
    }
    document.addEventListener('visibilitychange', onVisibilityChange);
    return () => {
      document.removeEventListener('visibilitychange', onVisibilityChange);
      stopPolling();
    };
  }, [libraryId, fetchOnce]);

  const refresh = useCallback(async () => {
    await fetchOnce('refresh');
  }, [fetchOnce]);

  return {
    data,
    error,
    isInitialLoading,
    isRefreshing,
    lastUpdatedAt,
    refresh,
  };
}
