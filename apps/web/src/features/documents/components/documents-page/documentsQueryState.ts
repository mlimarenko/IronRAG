import { useCallback, useEffect, useMemo, useState, type SetStateAction } from "react";

import {
  LIST_POLL_GRACE_MS,
  LIST_POLL_INTERVAL_MS,
  SEARCH_DEBOUNCE_MS,
  type DocumentsStatusBucket,
  type PageSizeOption,
  type SortValue,
} from "./documentsPageState";

type CursorSetter = (action: SetStateAction<(string | null)[]>) => void;

export function useDebouncedSearch(searchQuery: string) {
  const [debouncedSearch, setDebouncedSearch] = useState(searchQuery);
  useEffect(() => {
    const id = window.setTimeout(
      () => setDebouncedSearch(searchQuery),
      SEARCH_DEBOUNCE_MS,
    );
    return () => window.clearTimeout(id);
  }, [searchQuery]);
  return debouncedSearch;
}

export function useCursorStack({
  activeLibraryId,
  debouncedSearch,
  pageSize,
  sortValue,
  statusBucket,
}: {
  activeLibraryId: string | null;
  debouncedSearch: string;
  pageSize: PageSizeOption;
  sortValue: SortValue;
  statusBucket: DocumentsStatusBucket;
}) {
  const cursorScopeKey = useMemo(
    () =>
      JSON.stringify([
        activeLibraryId,
        debouncedSearch,
        sortValue,
        statusBucket,
        pageSize,
      ]),
    [activeLibraryId, debouncedSearch, pageSize, sortValue, statusBucket],
  );
  const [cursorStackState, setCursorStackState] = useState<{
    scopeKey: string;
    stack: (string | null)[];
  }>({ scopeKey: "", stack: [null] });
  const cursorStack =
    cursorStackState.scopeKey === cursorScopeKey
      ? cursorStackState.stack
      : [null];
  const setCursorStack = useCallback(
    (action: SetStateAction<(string | null)[]>) => {
      setCursorStackState((prev) => {
        const currentStack =
          prev.scopeKey === cursorScopeKey ? prev.stack : [null];
        const stack =
          typeof action === "function" ? action(currentStack) : action;
        return { scopeKey: cursorScopeKey, stack };
      });
    },
    [cursorScopeKey],
  );
  return { cursorStack, setCursorStack };
}

export function useListPollGrace() {
  const [listPollGraceUntil, setListPollGraceUntil] = useState(0);
  const [listPollClockMs, setListPollClockMs] = useState(0);
  const activateListPollGrace = useCallback(() => {
    const now = Date.now();
    setListPollClockMs(now);
    setListPollGraceUntil(now + LIST_POLL_GRACE_MS);
  }, []);
  useEffect(() => {
    if (listPollGraceUntil <= listPollClockMs) return;
    const timeoutMs = Math.min(
      LIST_POLL_INTERVAL_MS,
      listPollGraceUntil - listPollClockMs,
    );
    const timeoutId = window.setTimeout(
      () => setListPollClockMs(Date.now()),
      timeoutMs,
    );
    return () => window.clearTimeout(timeoutId);
  }, [listPollClockMs, listPollGraceUntil]);
  return {
    activateListPollGrace,
    shouldPoll: listPollGraceUntil > listPollClockMs,
  };
}

export function buildPaginationState({
  cursorStack,
  filteredTotal,
  isLoading,
  itemCount,
  nextCursor,
  pageSize,
  setCursorStack,
}: {
  cursorStack: (string | null)[];
  filteredTotal: number | null;
  isLoading: boolean;
  itemCount: number;
  nextCursor: string | null;
  pageSize: PageSizeOption;
  setCursorStack: CursorSetter;
}) {
  const currentPageNumber = cursorStack.length;
  const visibleRangeStart =
    itemCount === 0 ? 0 : (currentPageNumber - 1) * pageSize + 1;
  const visibleRangeEnd =
    itemCount === 0 ? 0 : (currentPageNumber - 1) * pageSize + itemCount;
  return {
    canGoNext: nextCursor != null && !isLoading,
    canGoPrevious: cursorStack.length > 1 && !isLoading,
    currentPageNumber,
    goToNextPage: () => {
      if (!nextCursor || isLoading) return;
      setCursorStack((prev) => [...prev, nextCursor]);
    },
    goToPreviousPage: () => {
      if (cursorStack.length <= 1 || isLoading) return;
      setCursorStack((prev) => prev.slice(0, -1));
    },
    goToPage: (target: number) => {
      if (isLoading) return;
      if (target < 1) return;
      if (target === currentPageNumber) return;
      if (target <= cursorStack.length) {
        setCursorStack((prev) => prev.slice(0, target));
        return;
      }
      if (target === cursorStack.length + 1 && nextCursor) {
        setCursorStack((prev) => [...prev, nextCursor]);
      }
    },
    pageSize,
    show:
      itemCount > 0 ||
      cursorStack.length > 1 ||
      nextCursor != null ||
      (filteredTotal ?? 0) > 0,
    totalPages:
      filteredTotal != null && filteredTotal > 0
        ? Math.max(1, Math.ceil(filteredTotal / pageSize))
        : null,
    visibleRangeEnd,
    visibleRangeStart,
  };
}
