import { useCallback, useMemo } from "react";
import type { Dispatch, SetStateAction } from "react";
import { useSearchParams } from "react-router-dom";

import type {
  AsyncOperationDetail,
  CatalogLibraryResponse,
  DocumentListSortKey,
  DocumentListSortOrder,
  DocumentListStatusFilter,
  WebIngestPattern,
  WebIngestUrlFilter,
} from "@/shared/api";
import type { DocumentListStatusCounts } from "@/shared/api/generated";
import {
  isStorageRecord,
  parseNumberOption,
  parseTableSort,
  useTableState,
  type TableSortState,
} from "@/shared/hooks/useTableState";

import { formatWebIngestPatterns } from "@/features/documents/model/webIngestPatterns";

export const PAGE_SIZE_OPTIONS = [50, 100, 250, 1000] as const;
export type PageSizeOption = (typeof PAGE_SIZE_OPTIONS)[number];
export const DEFAULT_PAGE_SIZE: PageSizeOption = 50;

export const SEARCH_DEBOUNCE_MS = 300;
export const SELECTED_DETAIL_REFRESH_MS = 5000;
export const LIST_POLL_GRACE_MS = 60_000;
export const LIST_POLL_INTERVAL_MS = 2500;

export type DocumentsPageTab = "documents" | "web";

export type DocumentsStatusBucket =
  | "all"
  | "ready"
  | "processing"
  | "queued"
  | "failed"
  | "canceled";

export const BUCKET_TO_BACKEND: Record<
  Exclude<DocumentsStatusBucket, "all">,
  DocumentListStatusFilter[]
> = {
  ready: ["ready"],
  processing: ["processing"],
  queued: ["queued"],
  failed: ["failed"],
  canceled: ["canceled"],
};

export type SortValue = `${DocumentListSortKey}:${DocumentListSortOrder}`;

const SORT_VALUES: readonly SortValue[] = [
  "uploaded_at:desc",
  "uploaded_at:asc",
  "file_name:asc",
  "file_name:desc",
  "file_type:asc",
  "file_type:desc",
  "file_size:asc",
  "file_size:desc",
  "status:asc",
  "status:desc",
];

const SORT_PARTS: Record<
  SortValue,
  { sortBy: DocumentListSortKey; sortOrder: DocumentListSortOrder }
> = {
  "uploaded_at:desc": { sortBy: "uploaded_at", sortOrder: "desc" },
  "uploaded_at:asc": { sortBy: "uploaded_at", sortOrder: "asc" },
  "file_name:asc": { sortBy: "file_name", sortOrder: "asc" },
  "file_name:desc": { sortBy: "file_name", sortOrder: "desc" },
  "file_type:asc": { sortBy: "file_type", sortOrder: "asc" },
  "file_type:desc": { sortBy: "file_type", sortOrder: "desc" },
  "file_size:asc": { sortBy: "file_size", sortOrder: "asc" },
  "file_size:desc": { sortBy: "file_size", sortOrder: "desc" },
  "status:asc": { sortBy: "status", sortOrder: "asc" },
  "status:desc": { sortBy: "status", sortOrder: "desc" },
};

export type WebIngestFilterSnapshot = {
  allowPatterns: WebIngestPattern[];
  blockPatterns: WebIngestPattern[];
  allowText: string;
  blockText: string;
};

export type WebIngestPolicySnapshot = {
  crawlFilter: WebIngestFilterSnapshot;
  materializationFilter: WebIngestFilterSnapshot;
};

export type WebIngestPolicyDraft = {
  libraryId: string;
  crawlAllowText: string;
  crawlBlockText: string;
  materializationAllowText: string;
  materializationBlockText: string;
};

export type UploadQueueItem = {
  name: string;
  state: "uploading" | "done" | "error";
  error?: string;
};

export type BulkRerunState = {
  kind: "delete" | "reprocess";
  operationId: string;
  total: number;
  completed: number;
  failed: number;
  inFlight: number;
  status: AsyncOperationDetail["status"];
};

export type LocalSortKey = "cost" | "time" | "finished";
export type LocalSortState = TableSortState<LocalSortKey>;

export type DocumentsTableState = {
  pageSize: PageSizeOption;
  sort: TableSortState<DocumentListSortKey>;
  localSort: LocalSortState;
};

export type UpdateSearchParamState = (
  updates: Record<string, string | null>,
) => void;

const DOCUMENTS_TABLE_ID = "documents.list";
const DEFAULT_SORT_VALUE: SortValue = "uploaded_at:desc";
const DEFAULT_DOCUMENTS_TABLE_STATE: DocumentsTableState = {
  pageSize: DEFAULT_PAGE_SIZE,
  sort: { key: "uploaded_at", direction: "desc" },
  localSort: null,
};

const DOCUMENT_SORT_KEYS: readonly DocumentListSortKey[] = [
  "uploaded_at",
  "file_name",
  "file_type",
  "file_size",
  "status",
];

const DOCUMENT_LOCAL_SORT_KEYS: readonly LocalSortKey[] = [
  "cost",
  "time",
  "finished",
];

function isPageSizeOption(value: number): value is PageSizeOption {
  return PAGE_SIZE_OPTIONS.some((option) => option === value);
}

function parsePageSize(
  value: string | null,
  fallback: PageSizeOption = DEFAULT_PAGE_SIZE,
): PageSizeOption {
  const parsed = Number.parseInt(value ?? "", 10);
  return isPageSizeOption(parsed) ? parsed : fallback;
}

function parseStatusBucket(value: string | null): DocumentsStatusBucket {
  if (
    value === "ready" ||
    value === "processing" ||
    value === "queued" ||
    value === "failed" ||
    value === "canceled"
  ) {
    return value;
  }
  return "all";
}

export function parseSortValue(
  raw: string | null,
  fallback: SortValue = DEFAULT_SORT_VALUE,
): SortValue {
  return SORT_VALUES.find((value) => value === raw) ?? fallback;
}

export function splitSortValue(sort: SortValue): {
  sortBy: DocumentListSortKey;
  sortOrder: DocumentListSortOrder;
} {
  return SORT_PARTS[sort];
}

function sortPreferenceToSortValue(sort: TableSortState<DocumentListSortKey>): SortValue {
  if (!sort) return DEFAULT_SORT_VALUE;
  return parseSortValue(`${sort.key}:${sort.direction}`);
}

function sortValueToPreference(sort: SortValue): TableSortState<DocumentListSortKey> {
  const parts = splitSortValue(sort);
  return { key: parts.sortBy, direction: parts.sortOrder };
}

function parseDocumentsTableState(raw: unknown): DocumentsTableState {
  const record = isStorageRecord(raw) ? raw : {};
  return {
    pageSize: parseNumberOption(
      record.pageSize,
      PAGE_SIZE_OPTIONS,
      DEFAULT_DOCUMENTS_TABLE_STATE.pageSize,
    ),
    sort: parseTableSort(
      record.sort,
      DOCUMENT_SORT_KEYS,
      DEFAULT_DOCUMENTS_TABLE_STATE.sort,
    ),
    localSort: parseTableSort(
      record.localSort,
      DOCUMENT_LOCAL_SORT_KEYS,
      DEFAULT_DOCUMENTS_TABLE_STATE.localSort,
    ),
  };
}

export function useDocumentsTableState(): [
  DocumentsTableState,
  Dispatch<SetStateAction<DocumentsTableState>>,
] {
  return useTableState<DocumentsTableState>({
    tableId: DOCUMENTS_TABLE_ID,
    defaultValue: DEFAULT_DOCUMENTS_TABLE_STATE,
    parse: parseDocumentsTableState,
  });
}

function toWebIngestFilterSnapshot(
  filter: WebIngestUrlFilter | undefined,
): WebIngestFilterSnapshot {
  const allowPatterns = filter?.allowPatterns ?? [];
  const blockPatterns = filter?.blockPatterns ?? [];
  return {
    allowPatterns,
    blockPatterns,
    allowText: formatWebIngestPatterns(allowPatterns),
    blockText: formatWebIngestPatterns(blockPatterns),
  };
}

export function extractWebIngestPolicy(
  library: CatalogLibraryResponse | null | undefined,
): WebIngestPolicySnapshot {
  const policy = library?.webIngestPolicy;
  return {
    crawlFilter: toWebIngestFilterSnapshot(policy?.crawlFilter),
    materializationFilter: toWebIngestFilterSnapshot(policy?.materializationFilter),
  };
}

export function getFilteredTotal(
  statusBucket: DocumentsStatusBucket,
  statusCounts: DocumentListStatusCounts | null,
  totalCount: number | null,
): number | null {
  if (statusCounts == null) return totalCount;
  switch (statusBucket) {
    case "all":
      return statusCounts.total;
    case "ready":
      return statusCounts.ready;
    case "processing":
      return statusCounts.processing;
    case "queued":
      return statusCounts.queued;
    case "failed":
      return statusCounts.failed;
    case "canceled":
      return statusCounts.canceled;
  }
}

export function parseCost(value: string | undefined | null): number | null {
  const parsed = Number.parseFloat(value ?? "");
  return Number.isNaN(parsed) ? null : parsed;
}

export function getErrorMessage(error: unknown, fallback: string): string {
  return error instanceof Error && error.message ? error.message : fallback;
}

export function useDocumentsPageUrlState({
  tableState,
  setTableState,
}: {
  tableState: DocumentsTableState;
  setTableState: Dispatch<SetStateAction<DocumentsTableState>>;
}) {
  const [searchParams, setSearchParams] = useSearchParams();
  const searchQuery = searchParams.get("q") ?? "";
  const sortValue = parseSortValue(
    searchParams.get("sort"),
    sortPreferenceToSortValue(tableState.sort),
  );
  const selectedDocumentId = searchParams.get("documentId");
  const statusBucket = parseStatusBucket(searchParams.get("status"));
  const pageSize = parsePageSize(searchParams.get("pageSize"), tableState.pageSize);
  const statusBackendFilter = useMemo(
    () => (statusBucket === "all" ? [] : BUCKET_TO_BACKEND[statusBucket]),
    [statusBucket],
  );
  const updateSearchParamState = useCallback(
    (updates: Record<string, string | null>) => {
      const next = new URLSearchParams(searchParams);
      for (const [key, value] of Object.entries(updates)) {
        if (value == null || value === "") {
          next.delete(key);
        } else {
          next.set(key, value);
        }
      }
      if (Object.prototype.hasOwnProperty.call(updates, "pageSize")) {
        setTableState((prev) => ({
          ...prev,
          pageSize: parsePageSize(updates.pageSize, DEFAULT_PAGE_SIZE),
        }));
      }
      if (Object.prototype.hasOwnProperty.call(updates, "sort")) {
        setTableState((prev) => ({
          ...prev,
          sort: sortValueToPreference(parseSortValue(updates.sort, DEFAULT_SORT_VALUE)),
        }));
      }
      setSearchParams(next, { replace: true });
    },
    [searchParams, setSearchParams, setTableState],
  );

  return {
    pageSize,
    searchQuery,
    selectedDocumentId,
    sortValue,
    statusBackendFilter,
    statusBucket,
    updateSearchParamState,
  };
}
