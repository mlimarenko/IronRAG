import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { Dispatch, SetStateAction } from "react";
import type { TFunction } from "i18next";
import { toast } from "sonner";

import {
  ASYNC_OPERATION_TERMINAL_STATES,
  documentsApi,
  type DocumentListPageResponse,
  type DocumentListSortKey,
  type DocumentListSortOrder,
  type DocumentListStatusFilter,
} from "@/shared/api";
import type { DocumentItem, Library } from "@/shared/types";

import { getDocumentProcessingDurationMs } from "@/features/documents/model/documentAdapter";

import {
  splitSortValue,
  type BulkRerunState,
  type DocumentsTableState,
  type LocalSortKey,
  type LocalSortState,
  type PageSizeOption,
  type SortValue,
  type UpdateSearchParamState,
} from "./documentsPageState";
import { useBulkRerunProgressQuery } from "./useBulkRerunProgressQuery";

type DocumentsListControllerInput = {
  activeLibrary: Library;
  activateListPollGrace: () => void;
  debouncedSearch: string;
  errorMessage: (error: unknown, fallback: string) => string;
  filteredTotal: number | null;
  items: DocumentItem[];
  loadFirstPage: () => Promise<void>;
  localSort: LocalSortState;
  onSelectionModeChange: (selectionMode: boolean) => void;
  pageSize: PageSizeOption;
  setTableState: Dispatch<SetStateAction<DocumentsTableState>>;
  sortBy: DocumentListSortKey;
  sortOrder: DocumentListSortOrder;
  sortValue: SortValue;
  statusBackendFilter: DocumentListStatusFilter[];
  t: TFunction;
  updateSearchParamState: UpdateSearchParamState;
};

export function useDocumentsListController({
  activeLibrary,
  activateListPollGrace,
  debouncedSearch,
  errorMessage,
  filteredTotal,
  items,
  loadFirstPage,
  localSort,
  onSelectionModeChange,
  pageSize,
  setTableState,
  sortBy,
  sortOrder,
  sortValue,
  statusBackendFilter,
  t,
  updateSearchParamState,
}: DocumentsListControllerInput) {
  const [selectionMode, setSelectionMode] = useState(false);
  const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set());
  const [expandingSelection, setExpandingSelection] = useState(false);
  const [bulkRerun, setBulkRerun] = useState<BulkRerunState | null>(null);
  const [processingClockMs, setProcessingClockMs] = useState(() => Date.now());
  const terminalToastFiredRef = useRef(false);
  const bulkQuery = useBulkRerunProgressQuery(bulkRerun);

  const clearSelection = useCallback(() => {
    setSelectedIds(new Set());
    setSelectionMode(false);
    setExpandingSelection(false);
  }, []);
  useEffect(() => onSelectionModeChange(selectionMode), [
    onSelectionModeChange,
    selectionMode,
  ]);
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape" && selectionMode) clearSelection();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [clearSelection, selectionMode]);

  const hasInFlightDocs = useMemo(
    () => items.some((doc) => doc.status === "queued" || doc.status === "processing"),
    [items],
  );
  useEffect(() => {
    if (!hasInFlightDocs) return undefined;
    const id = window.setInterval(() => setProcessingClockMs(Date.now()), 1000);
    return () => window.clearInterval(id);
  }, [hasInFlightDocs]);

  const displayedItems = useMemo(() => {
    if (!localSort) return items;
    const direction = localSort.direction === "asc" ? 1 : -1;
    const score = (doc: DocumentItem) => {
      if (localSort.key === "cost") return doc.cost ?? -Infinity;
      if (localSort.key === "time") {
        return getDocumentProcessingDurationMs(doc, processingClockMs) ?? -Infinity;
      }
      return doc.processingFinishedAt
        ? Date.parse(doc.processingFinishedAt)
        : -Infinity;
    };
    return [...items].sort((left, right) => {
      const lhs = score(left);
      const rhs = score(right);
      if (lhs === rhs) return 0;
      return lhs < rhs ? -1 * direction : direction;
    });
  }, [items, localSort, processingClockMs]);

  const toggleSelection = useCallback((id: string) => {
    setSelectedIds((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }, []);
  const onSortChange = useCallback(
    (value: SortValue) =>
      updateSearchParamState({
        sort: value === "uploaded_at:desc" ? null : value,
        documentId: null,
      }),
    [updateSearchParamState],
  );
  const toggleSortDirection = useCallback(
    (target: DocumentListSortKey) => {
      if (sortBy !== target) {
        onSortChange(`${target}:${sortOrder}`);
        return;
      }
      onSortChange(`${target}:${sortOrder === "asc" ? "desc" : "asc"}`);
    },
    [onSortChange, sortBy, sortOrder],
  );
  const toggleLocalSort = useCallback(
    (key: LocalSortKey) => {
      setTableState((prev) => ({
        ...prev,
        localSort:
          prev.localSort && prev.localSort.key === key
            ? { key, direction: prev.localSort.direction === "asc" ? "desc" : "asc" }
            : { key, direction: "desc" },
      }));
    },
    [setTableState],
  );

  const selectAllMatching = useCallback(async () => {
    if (expandingSelection) return;
    setExpandingSelection(true);
    try {
      const split = splitSortValue(sortValue);
      const collected = new Set(selectedIds);
      let cursor: string | null | undefined;
      while (collected.size < 100_000) {
        const page: DocumentListPageResponse = await documentsApi.list({
          libraryId: activeLibrary.id,
          limit: pageSize,
          sortBy: split.sortBy,
          sortOrder: split.sortOrder,
          includeTotal: false,
          ...(cursor ? { cursor } : {}),
          ...(debouncedSearch ? { search: debouncedSearch } : {}),
          ...(statusBackendFilter.length > 0 ? { status: statusBackendFilter } : {}),
        });
        for (const row of page.items) collected.add(row.id);
        if (!page.nextCursor) break;
        cursor = page.nextCursor;
      }
      setSelectedIds(collected);
    } catch (err) {
      toast.error(errorMessage(err, t("documents.failedToLoad")));
    } finally {
      setExpandingSelection(false);
    }
  }, [
    activeLibrary.id,
    debouncedSearch,
    errorMessage,
    expandingSelection,
    pageSize,
    selectedIds,
    sortValue,
    statusBackendFilter,
    t,
  ]);

  const startBulkRerun = useCallback(
    (kind: BulkRerunState["kind"], operationId: string, total: number) => {
      activateListPollGrace();
      clearSelection();
      terminalToastFiredRef.current = false;
      setBulkRerun({ kind, operationId, total, completed: 0, failed: 0, inFlight: total, status: "processing" });
    },
    [activateListPollGrace, clearSelection],
  );
  const handleBulkDelete = useCallback(async () => {
    if (!confirm(t("documents.confirmBulkDelete", { count: selectedIds.size }))) return;
    const ids = Array.from(selectedIds);
    if (ids.length === 0) return;
    try {
      const accepted = await documentsApi.batchDelete(ids);
      startBulkRerun("delete", accepted.batchOperationId, accepted.total);
    } catch (err) {
      toast.error(errorMessage(err, t("documents.bulkDeleteFailed")));
    }
  }, [errorMessage, selectedIds, startBulkRerun, t]);
  const handleBulkCancel = useCallback(async () => {
    try {
      await documentsApi.batchCancel(Array.from(selectedIds));
      toast.success(t("documents.bulkCancelSuccess", { count: selectedIds.size }));
      clearSelection();
      await loadFirstPage();
    } catch {
      toast.error(t("documents.bulkCancelFailed"));
    }
  }, [clearSelection, loadFirstPage, selectedIds, t]);
  const handleBulkReprocess = useCallback(async () => {
    const ids = Array.from(selectedIds);
    if (ids.length === 0) return;
    try {
      const accepted = await documentsApi.batchReprocess(ids);
      startBulkRerun("reprocess", accepted.batchOperationId, accepted.total);
    } catch {
      toast.error(t("documents.bulkReprocessFailed"));
    }
  }, [selectedIds, startBulkRerun, t]);

  useEffect(() => {
    const detail = bulkQuery.operationQuery.data;
    if (!detail || !bulkRerun) return;
    if (!ASYNC_OPERATION_TERMINAL_STATES.has(detail.status) || terminalToastFiredRef.current) return;
    terminalToastFiredRef.current = true;
    const isDelete = bulkRerun.kind === "delete";
    if (detail.status === "ready") {
      toast.success(
        isDelete
          ? t("documents.bulkDeleteSuccess", { count: detail.progress.completed })
          : t("documents.bulkReprocessSuccess", { count: detail.progress.completed }),
      );
    } else if (detail.progress.completed > 0) {
      toast.warning(
        isDelete
          ? t("documents.bulkDeletePartial", {
              ok: detail.progress.completed,
              failed: detail.progress.failed,
            })
          : t("documents.bulkReprocessPartial", {
              ok: detail.progress.completed,
              failed: detail.progress.failed,
            }),
      );
    } else {
      toast.error(
        isDelete
          ? t("documents.bulkDeleteAllFailed", { count: detail.progress.failed })
          : t("documents.bulkReprocessAllFailed", { count: detail.progress.failed }),
      );
    }
    void loadFirstPage();
    const timeoutId = window.setTimeout(() => setBulkRerun(null), 4000);
    return () => window.clearTimeout(timeoutId);
  }, [bulkQuery.operationQuery.data, bulkRerun, loadFirstPage, t]);

  return {
    bulkProgress: bulkQuery.progress,
    clearSelection,
    displayedItems,
    expandingSelection,
    handleBulkCancel,
    handleBulkDelete,
    handleBulkReprocess,
    localSort,
    processingClockMs,
    selectAllMatching,
    selectedIds,
    selectionMode,
    setBulkRerun,
    setSelectedIds,
    setSelectionMode,
    showSelectAllMatching:
      selectionMode &&
      items.length > 0 &&
      items.every((doc) => selectedIds.has(doc.id)) &&
      filteredTotal != null &&
      filteredTotal > items.length &&
      selectedIds.size < filteredTotal,
    toggleLocalSort,
    toggleSelection,
    toggleSortDirection,
  };
}
