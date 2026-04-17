import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { useSearchParams } from "react-router-dom";

import { useApp } from "@/contexts/AppContext";
import {
  apiFetch,
  billingApi,
  documentsApi,
  DOCUMENT_LIST_STATUS_FILTERS,
  opsApi,
  ASYNC_OPERATION_TERMINAL_STATES,
  type AsyncOperationDetail,
  type DocumentListPageResponse,
  type DocumentListSortKey,
  type DocumentListSortOrder,
  type DocumentListStatusFilter,
  type WebIngestRunListItem,
} from "@/api";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  ArrowDown,
  ArrowUp,
  Ban,
  CheckCircle2,
  Clock,
  File,
  FileText,
  Globe,
  Hourglass,
  Loader2,
  RotateCw,
  Search,
  Upload,
  XCircle,
  CheckSquare,
} from "lucide-react";

import type { DocumentItem, DocumentLifecycle } from "@/types";
import { compactText } from "@/lib/compactText";
import {
  buildDocumentStatusBadgeConfig,
  formatDate,
  formatDocumentTypeLabel,
  formatSize,
  getDocumentProcessingDurationMs,
  isWebPageDocument,
  mapListItem,
} from "@/adapters/documents";
import { DocumentsPageHeader } from "@/pages/documents/DocumentsPageHeader";
import { DocumentsInspectorPanel } from "@/pages/documents/DocumentsInspectorPanel";
import { DocumentsOverlays } from "@/pages/documents/DocumentsOverlays";
import { WebRunsPanel } from "@/pages/documents/WebRunsPanel";
import { BulkRerunProgressBanner } from "@/pages/documents/BulkRerunProgressBanner";
import {
  buildUploadCandidates,
  normalizeUploadName,
  type UploadCandidate,
} from "@/pages/documents/uploadCandidates";
import { DocumentEditorShell } from "@/pages/documents/editor/DocumentEditorShell";
import { isEditorEditableSourceFormat } from "@/pages/documents/editor/editorSurfaceMode";
import { useDocumentEditor } from "@/pages/documents/editor/useDocumentEditor";

const PAGE_SIZE_OPTIONS = [50, 100, 250, 1000] as const;
type PageSizeOption = (typeof PAGE_SIZE_OPTIONS)[number];
const DEFAULT_PAGE_SIZE: PageSizeOption = 50;

function parsePageSize(value: string | null): PageSizeOption {
  const parsed = Number.parseInt(value ?? "", 10);
  if (PAGE_SIZE_OPTIONS.includes(parsed as PageSizeOption)) {
    return parsed as PageSizeOption;
  }
  return DEFAULT_PAGE_SIZE;
}

/**
 * UI filter buckets shown as pills. Each bucket maps 1:1 to the canonical
 * backend `derived_status` enum produced by `list_document_page_rows` —
 * no client-side aggregation, so operator counts in the pill badges
 * always line up exactly with the rows the server returns when the
 * filter is applied. `all` is the only synthetic bucket.
 */
type DocumentsStatusBucket =
  | "all"
  | "ready"
  | "processing"
  | "queued"
  | "failed"
  | "canceled";

const BUCKET_TO_BACKEND: Record<
  Exclude<DocumentsStatusBucket, "all">,
  DocumentListStatusFilter[]
> = {
  ready: ["ready"],
  processing: ["processing"],
  queued: ["queued"],
  failed: ["failed"],
  canceled: ["canceled"],
};

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

const SEARCH_DEBOUNCE_MS = 300;
const SELECTED_DETAIL_REFRESH_MS = 5000;

type SortValue = `${DocumentListSortKey}:${DocumentListSortOrder}`;

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

function parseSortValue(raw: string | null): SortValue {
  if (raw && (SORT_VALUES as readonly string[]).includes(raw)) {
    return raw as SortValue;
  }
  return "uploaded_at:desc";
}

function splitSortValue(sort: SortValue): {
  sortBy: DocumentListSortKey;
  sortOrder: DocumentListSortOrder;
} {
  const [sortBy, sortOrder] = sort.split(":") as [
    DocumentListSortKey,
    DocumentListSortOrder,
  ];
  return { sortBy, sortOrder };
}

export default function DocumentsPage() {
  const { t } = useTranslation();
  const { activeLibrary, locale } = useApp();
  const [searchParams, setSearchParams] = useSearchParams();

  const searchQuery = searchParams.get("q") ?? "";
  const sortValue = parseSortValue(searchParams.get("sort"));
  const selectedDocumentId = searchParams.get("documentId");
  // `?status` stores the UI bucket name (`all`/`in_progress`/`ready`/
  // `failed`), not the raw backend values. On every API call we translate
  // the bucket via `BUCKET_TO_BACKEND` so the backend always sees one of
  // its canonical `derived_status` values. This keeps the URL short and
  // stable across UX refactors.
  const statusBucket = parseStatusBucket(searchParams.get("status"));
  const statusBackendFilter: DocumentListStatusFilter[] = useMemo(
    () => (statusBucket === "all" ? [] : BUCKET_TO_BACKEND[statusBucket]),
    [statusBucket],
  );
  const statusBucketKey = statusBucket;
  const pageSize: PageSizeOption = parsePageSize(searchParams.get("pageSize"));

  // ----- List state -----
  const [items, setItems] = useState<DocumentItem[]>([]);
  // Cursor history for keyset pagination. Index 0 is always `null` (first
  // page); subsequent entries are the cursors passed back by the server for
  // each page the operator navigated through. Next pushes, Previous pops.
  // Filter/sort/library/pageSize change resets the stack to `[null]`.
  const [cursorStack, setCursorStack] = useState<(string | null)[]>([null]);
  const [nextCursor, setNextCursor] = useState<string | null>(null);
  const [totalCount, setTotalCount] = useState<number | null>(null);
  const [statusCounts, setStatusCounts] = useState<{
    total: number;
    ready: number;
    processing: number;
    queued: number;
    failed: number;
    canceled: number;
  } | null>(null);
  // `isLoading` covers the first-render / filter-change path — during that
  // window the table is hidden behind a full-screen spinner because there is
  // genuinely nothing yet to show.
  //
  // `isRefreshing` covers every subsequent fetch (Prev/Next, polling the
  // queue, explicit Refresh, post-mutation reload). It does NOT blank the
  // table — the old rows stay mounted, click targets stay stable, and the
  // inspector selection is preserved. Without this split, the polling
  // effect flashed the loader every 2.5 s: the whole table was replaced
  // with a spinner for ~100 ms and any click that landed during that
  // window hit the spinner instead of a row.
  const [isLoading, setIsLoading] = useState(false);
  const [isRefreshing, setIsRefreshing] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [costMap, setCostMap] = useState<Record<string, number>>({});
  const [libraryTotalCost, setLibraryTotalCost] = useState<number | null>(null);

  // Debounced search — the query string updates immediately for shareable
  // URLs, but the actual API fetch waits 300ms so keystrokes don't trigger
  // a cascade of requests.
  const [debouncedSearch, setDebouncedSearch] = useState(searchQuery);
  useEffect(() => {
    const id = window.setTimeout(
      () => setDebouncedSearch(searchQuery),
      SEARCH_DEBOUNCE_MS,
    );
    return () => window.clearTimeout(id);
  }, [searchQuery]);

  // Shadow ref of the latest items list so memoized callbacks (fetchPage,
  // handlers) can branch on "do we already have rows?" without reading
  // stale closure state.
  const itemsRef = useRef<DocumentItem[]>([]);

  // ----- Selection (inspector) -----
  const [selectedDoc, setSelectedDoc] = useState<DocumentItem | null>(null);
  const selectedDocRef = useRef<DocumentItem | null>(null);
  useEffect(() => {
    selectedDocRef.current = selectedDoc;
  }, [selectedDoc]);
  const [inspectorSegments, setInspectorSegments] = useState<number | null>(
    null,
  );
  const [inspectorFacts, setInspectorFacts] = useState<number | null>(null);
  const [inspectorLifecycle, setInspectorLifecycle] =
    useState<DocumentLifecycle | null>(null);

  // ----- Overlays & upload state -----
  const [addLinkOpen, setAddLinkOpen] = useState(false);
  const [deleteDocOpen, setDeleteDocOpen] = useState(false);
  const [replaceFileOpen, setReplaceFileOpen] = useState(false);
  const [dragOver, setDragOver] = useState(false);
  const [uploadQueue, setUploadQueue] = useState<
    { name: string; state: "uploading" | "done" | "error"; error?: string }[]
  >([]);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const folderInputRef = useRef<HTMLInputElement>(null);
  const [duplicateConflict, setDuplicateConflict] = useState<{
    candidate: UploadCandidate;
    existingDocId: string;
    remaining: UploadCandidate[];
  } | null>(null);
  const [replaceFile, setReplaceFile] = useState<File | null>(null);
  const [replaceLoading, setReplaceLoading] = useState(false);
  const replaceFileInputRef = useRef<HTMLInputElement>(null);

  // ----- Web ingest form & runs -----
  const [seedUrl, setSeedUrl] = useState("");
  const [crawlMode, setCrawlMode] = useState("recursive_crawl");
  const [boundaryPolicy, setBoundaryPolicy] = useState("same_host");
  const [maxDepth, setMaxDepth] = useState("3");
  const [maxPages, setMaxPages] = useState("100");
  const [webIngestLoading, setWebIngestLoading] = useState(false);
  const [activeTab, setActiveTab] = useState<"documents" | "web">("documents");
  const [webRuns, setWebRuns] = useState<WebIngestRunListItem[]>([]);

  // ----- Bulk selection -----
  const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set());
  const [selectionMode, setSelectionMode] = useState(false);
  // Tracks a multi-page fetch that expands the selection across every
  // row matching the current filter (not just the visible page). The
  // header checkbox only ever toggles the rows on screen — when the
  // filter has more rows than one page, a banner offers "select all N"
  // which kicks off the expansion below.
  const [expandingSelection, setExpandingSelection] = useState(false);

  // ----- Async batch rerun progress -----
  //
  // Canonical progress indicator for `POST /content/documents/batch-reprocess`.
  // The server returns 202 immediately with a parent `ops_async_operation` id;
  // the UI polls that id until it enters a terminal state. One tiny inline
  // progress block — no modal — keeps the documents surface the primary
  // work area while the rerun is in flight.
  const [bulkRerun, setBulkRerun] = useState<{
    operationId: string;
    total: number;
    completed: number;
    failed: number;
    inFlight: number;
    status: AsyncOperationDetail["status"];
  } | null>(null);
  const bulkRerunPollRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const bulkRerunAbortedRef = useRef(false);

  useEffect(
    () => () => {
      bulkRerunAbortedRef.current = true;
      if (bulkRerunPollRef.current) {
        clearTimeout(bulkRerunPollRef.current);
        bulkRerunPollRef.current = null;
      }
    },
    [],
  );

  const statusBadgeConfig = useMemo(
    () => buildDocumentStatusBadgeConfig(t),
    [t],
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
      setSearchParams(next, { replace: true });
    },
    [searchParams, setSearchParams],
  );

  const errorMessage = useCallback(
    (error: unknown, fallback: string) =>
      error instanceof Error && error.message ? error.message : fallback,
    [],
  );

  const editAvailability = useCallback(
    (doc: DocumentItem | null) => {
      if (!doc) {
        return { enabled: false, reason: null as string | null };
      }
      if (!isEditorEditableSourceFormat(doc.fileType)) {
        return { enabled: false, reason: t("documents.editUnavailableFormat") };
      }
      if (
        doc.readiness === "readable" ||
        doc.readiness === "graph_sparse" ||
        doc.readiness === "graph_ready"
      ) {
        return { enabled: true, reason: null as string | null };
      }
      if (doc.readiness === "processing") {
        return {
          enabled: false,
          reason: t("documents.editUnavailableProcessing"),
        };
      }
      if (doc.readiness === "failed") {
        return { enabled: false, reason: t("documents.editUnavailableFailed") };
      }
      return { enabled: false, reason: t("documents.editUnavailableGeneric") };
    },
    [t],
  );

  // ----- List fetching -----

  /**
   * Canonical page fetch. Replaces `items` with the page returned by the
   * server for the given cursor. Called with `null` for the first page
   * (and whenever filters change) and with `nextCursor` / a popped stack
   * entry for explicit Prev/Next navigation.
   *
   * The `refreshTotal` flag triggers the opt-in `COUNT(*)` request used
   * to populate the "Page X of Y" footer — it's only sent on the very
   * first page load per filter set so we don't pay the count cost on
   * every pagination click.
   */
  const fetchPage = useCallback(
    async (cursor: string | null, refreshTotal: boolean) => {
      if (!activeLibrary) return;
      // First render (no items yet) must show the big loader, every other
      // fetch goes through the non-blanking refresh path so the table /
      // inspector never flash away mid-interaction. We read the items
      // count off of the ref rather than closing over state: `fetchPage`
      // is a memoized callback whose closure would otherwise keep the
      // first-mount `[]` value alive forever.
      const isFirstLoad = itemsRef.current.length === 0;
      if (isFirstLoad) {
        setIsLoading(true);
      } else {
        setIsRefreshing(true);
      }
      setLoadError(null);
      const { sortBy, sortOrder } = splitSortValue(sortValue);
      try {
        // `includeTotal` triggers a separate library-wide aggregate CTE on
        // the backend that is O(documents) and trips the >1 s slow-query
        // threshold on reference-sized libraries. We pay it on the first
        // load per filter set and on explicit refreshes (upload, batch
        // action, manual reload) — the polling effect (2.5 s tick) reuses
        // the previous counts. They snap on the next filter change or
        // explicit reload, which is far cheaper than recomputing the
        // aggregate every 2.5 s during a long batch rerun.
        const [page, costs, libraryCostSummary] = await Promise.all([
          documentsApi.list({
            libraryId: activeLibrary.id,
            limit: pageSize,
            cursor: cursor ?? undefined,
            search: debouncedSearch || undefined,
            sortBy,
            sortOrder,
            includeTotal: refreshTotal,
            status:
              statusBackendFilter.length > 0 ? statusBackendFilter : undefined,
          }),
          refreshTotal
            ? billingApi
                .getLibraryDocumentCosts(activeLibrary.id)
                .catch(() => [])
            : Promise.resolve(null),
          refreshTotal
            ? billingApi
                .getLibraryCostSummary(activeLibrary.id)
                .catch(() => null)
            : Promise.resolve(null),
        ]);
        const mapped = page.items.map((raw) => mapListItem(raw, t));
        if (costs) {
          const newCostMap: Record<string, number> = {};
          for (const c of costs) {
            const parsed = parseFloat(c.totalCost);
            if (!Number.isNaN(parsed)) {
              newCostMap[c.documentId] = parsed;
            }
          }
          for (const doc of mapped) {
            const cost = newCostMap[doc.id];
            if (cost != null) doc.cost = cost;
          }
          setCostMap(newCostMap);
        } else {
          for (const doc of mapped) {
            const cost = costMap[doc.id];
            if (cost != null) doc.cost = cost;
          }
        }
        setItems(mapped);
        setNextCursor(page.nextCursor);
        // Counts are only present in the response when the backend was
        // asked for them via `includeTotal`. On a polling refresh we
        // intentionally skip the aggregate, so keep the previously
        // observed counts rather than blanking them.
        if (refreshTotal) {
          setTotalCount(page.totalCount ?? null);
          setStatusCounts(page.statusCounts ?? null);
          if (libraryCostSummary) {
            const parsed = parseFloat(libraryCostSummary.totalCost);
            if (!Number.isNaN(parsed)) {
              setLibraryTotalCost(parsed);
            }
          }
        }
      } catch (err) {
        setLoadError(errorMessage(err, t("documents.failedToLoad")));
        if (isFirstLoad) {
          setItems([]);
          setNextCursor(null);
        }
      } finally {
        if (isFirstLoad) {
          setIsLoading(false);
        } else {
          setIsRefreshing(false);
        }
      }
    },
    [
      activeLibrary,
      costMap,
      debouncedSearch,
      errorMessage,
      pageSize,
      sortValue,
      statusBackendFilter,
      t,
    ],
  );

  // Whenever library, search, sort, status filter, or page size changes,
  // reset the cursor stack to the first page and fetch. Explicit Prev/Next
  // never triggers this effect — it's the single source of truth for
  // "re-start the paginator".
  useEffect(() => {
    if (!activeLibrary) return;
    setCursorStack([null]);
    void fetchPage(null, true);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeLibrary, debouncedSearch, sortValue, statusBucketKey, pageSize]);

  const goToNextPage = useCallback(() => {
    if (!nextCursor || isLoading) return;
    setCursorStack((prev) => [...prev, nextCursor]);
    void fetchPage(nextCursor, false);
  }, [fetchPage, isLoading, nextCursor]);

  const goToPreviousPage = useCallback(() => {
    if (cursorStack.length <= 1 || isLoading) return;
    const nextStack = cursorStack.slice(0, -1);
    const target = nextStack[nextStack.length - 1] ?? null;
    setCursorStack(nextStack);
    void fetchPage(target, false);
  }, [cursorStack, fetchPage, isLoading]);

  const currentPageNumber = cursorStack.length;
  // Effective total for the Prev/Next footer. When a filter bucket is
  // active the denominator is the matching `statusCounts` slot rather
  // than the library-wide total, so "Page X of Y" stays honest.
  const filteredTotal = useMemo<number | null>(() => {
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
  }, [statusBucket, statusCounts, totalCount]);
  const totalPages =
    filteredTotal != null && filteredTotal > 0
      ? Math.max(1, Math.ceil(filteredTotal / pageSize))
      : null;
  const canGoPrevious = cursorStack.length > 1 && !isLoading;
  const canGoNext = nextCursor != null && !isLoading;
  const visibleRangeStart =
    items.length === 0 ? 0 : (currentPageNumber - 1) * pageSize + 1;
  const visibleRangeEnd =
    items.length === 0 ? 0 : (currentPageNumber - 1) * pageSize + items.length;

  /**
   * Reset the pagination stack and reload the first page. Used by every
   * callsite that just wants "re-fetch current library from the top" —
   * search/sort/filter changes have their own effect path; this helper
   * is what upload success, batch delete, manual refresh, etc. call.
   */
  const loadFirstPage = useCallback(async () => {
    setCursorStack([null]);
    await fetchPage(null, true);
  }, [fetchPage]);

  // Grace deadline for "keep polling the list even if the current rows
  // still look terminal". Retry / reprocess mutations set this to
  // `now + LIST_POLL_GRACE_MS` — the first re-fetch right after the
  // click usually still shows the stale terminal status (the backend
  // has not moved the row to `queued` yet), so we cannot rely on the
  // in-flight heuristic alone to keep the poll alive. While this
  // deadline is in the future the list effect polls regardless, then
  // the heuristic takes over once `queued`/`processing` actually
  // appears in the rows.
  const LIST_POLL_GRACE_MS = 60_000;
  const LIST_POLL_INTERVAL_MS = 2500;
  const [listPollGraceUntil, setListPollGraceUntil] = useState<number>(0);

  // Whole-list polling mode: while any row is `queued`/`processing`
  // (or we are inside the post-retry grace window) re-fetch the same
  // page every few seconds so the UI walks the row through
  // queued → processing → ready on its own, without requiring the
  // operator to reload. Stops automatically when all rows are
  // terminal and the grace deadline has passed, or on unmount /
  // library change / filter change (the fetchPage dep list below
  // rebuilds the effect).
  useEffect(() => {
    if (!activeLibrary) return;
    const hasInFlight = items.some(
      (doc) => doc.status === "queued" || doc.status === "processing",
    );
    const withinGrace = Date.now() < listPollGraceUntil;
    if (!hasInFlight && !withinGrace) return;
    const currentCursor = cursorStack[cursorStack.length - 1] ?? null;
    const id = window.setInterval(() => {
      void fetchPage(currentCursor, false);
    }, LIST_POLL_INTERVAL_MS);
    return () => window.clearInterval(id);
  }, [activeLibrary, items, listPollGraceUntil, cursorStack, fetchPage]);

  // Keep `itemsRef` aligned with the latest state so `fetchPage` can
  // distinguish first load vs background refresh without stale closures.
  useEffect(() => {
    itemsRef.current = items;
  }, [items]);

  // Sync the inspector's selected-doc snapshot from the latest `items`
  // after each list refresh. Without this the inspector stays frozen
  // on the pre-retry snapshot even though the table rows update under
  // it, because `selectedDoc` is a separate piece of state.
  useEffect(() => {
    const current = selectedDocRef.current;
    if (!current) return;
    const fresh = items.find((doc) => doc.id === current.id);
    if (!fresh) return;
    if (
      fresh.status !== current.status ||
      fresh.readinessKind !== current.readinessKind
    ) {
      setSelectedDoc(fresh);
    }
  }, [items]);

  // Web runs are owned by the web tab — load them alongside the first
  // documents page so the tab counter is accurate. Separate lifetime from
  // the documents list keeps the heavy list out of the web-tab path.
  useEffect(() => {
    if (!activeLibrary) return;
    let cancelled = false;
    documentsApi
      .listWebRuns(activeLibrary.id)
      .then((runs) => {
        if (cancelled) return;
        setWebRuns(runs);
      })
      .catch(() => {
        if (!cancelled) setWebRuns([]);
      });
    return () => {
      cancelled = true;
    };
  }, [activeLibrary]);

  // ----- Selected document detail refresh (inspector only) -----

  const fetchSelectedDetail = useCallback(async (documentId: string) => {
    try {
      const raw = await documentsApi.get(documentId);
      if (raw.lifecycle) {
        setInspectorLifecycle(raw.lifecycle as DocumentLifecycle);
      }
    } catch {
      // Inspector keeps whatever data it already has — the selection
      // row still renders from the list item, so a transient detail
      // failure is non-fatal.
    }
    const [segments, facts] = await Promise.all([
      documentsApi.getPreparedSegments(documentId).catch(() => []),
      documentsApi.getTechnicalFacts(documentId).catch(() => []),
    ]);
    if (selectedDocRef.current?.id !== documentId) return;
    setInspectorSegments(segments.length);
    setInspectorFacts(facts.length);
  }, []);

  const handleSelectDoc = useCallback(
    async (doc: DocumentItem, syncQuery = true) => {
      if (syncQuery) {
        updateSearchParamState({ documentId: doc.id });
      }
      setSelectedDoc(doc);
      setInspectorSegments(null);
      setInspectorFacts(null);
      setInspectorLifecycle(null);
      await fetchSelectedDetail(doc.id);
    },
    [fetchSelectedDetail, updateSearchParamState],
  );

  // Restore the inspector's selection from the `documentId` URL param.
  // The list item is the source of truth for the selected doc view —
  // when the row has not yet been loaded (because the user deep-linked to
  // a row that lives past the current window) we cannot select it.
  useEffect(() => {
    if (!selectedDocumentId) {
      setSelectedDoc(null);
      setInspectorSegments(null);
      setInspectorFacts(null);
      setInspectorLifecycle(null);
      return;
    }
    if (selectedDoc?.id === selectedDocumentId) return;
    const matched = items.find((doc) => doc.id === selectedDocumentId);
    if (matched) {
      void handleSelectDoc(matched, false);
    }
  }, [handleSelectDoc, items, selectedDoc?.id, selectedDocumentId]);

  // Quiet background refresh for the currently-selected document, but
  // only while the document is actually in flight. Terminal statuses
  // (`ready` / `failed` / `canceled`) never change without operator
  // action, so there is nothing to poll for — refreshing them every
  // 5 s just replays a ~1 s backend round-trip against a row that will
  // stay the same until the next retry/edit/delete click. That click
  // already triggers a fresh `fetchSelectedDetail` via its own handler,
  // so the inspector stays current without the timer.
  useEffect(() => {
    if (!selectedDoc) return;
    if (
      selectedDoc.status === "ready" ||
      selectedDoc.status === "failed" ||
      selectedDoc.status === "canceled"
    ) {
      return;
    }
    const interval = window.setInterval(() => {
      const currentId = selectedDocRef.current?.id;
      if (!currentId) return;
      void fetchSelectedDetail(currentId);
    }, SELECTED_DETAIL_REFRESH_MS);
    return () => window.clearInterval(interval);
  }, [selectedDoc, fetchSelectedDetail]);

  // ----- Upload pipeline -----

  const doUploadFile = useCallback(
    async (candidate: UploadCandidate) => {
      if (!activeLibrary) return;
      setUploadQueue((prev) => [
        ...prev,
        { name: candidate.name, state: "uploading" },
      ]);
      try {
        await documentsApi.upload(activeLibrary.id, candidate.file, {
          externalKey: candidate.name,
          fileName: candidate.file.name,
          title: candidate.name,
        });
        // Upload queues an ingest job that immediately lands in
        // `queued`; activate the whole-list polling grace window so
        // the new row walks through queued → processing → ready
        // without a manual refresh.
        setListPollGraceUntil(Date.now() + LIST_POLL_GRACE_MS);
        setUploadQueue((prev) =>
          prev.map((u) =>
            u.name === candidate.name ? { ...u, state: "done" } : u,
          ),
        );
      } catch (err) {
        const message = errorMessage(err, t("documents.uploadFailed"));
        setUploadQueue((prev) =>
          prev.map((u) =>
            u.name === candidate.name
              ? { ...u, state: "error", error: message }
              : u,
          ),
        );
      }
    },
    [activeLibrary, errorMessage, t],
  );

  const doReplaceFile = useCallback(
    async (docId: string, file: File, uploadName = file.name) => {
      setUploadQueue((prev) => [
        ...prev,
        { name: uploadName, state: "uploading" },
      ]);
      try {
        await documentsApi.replace(docId, file);
        // Replace also kicks off a fresh ingest job on the existing
        // document; keep the list polling the same way upload does.
        setListPollGraceUntil(Date.now() + LIST_POLL_GRACE_MS);
        setUploadQueue((prev) =>
          prev.map((u) =>
            u.name === uploadName ? { ...u, state: "done" } : u,
          ),
        );
      } catch (err) {
        const message = errorMessage(err, t("documents.replaceFileFailed"));
        setUploadQueue((prev) =>
          prev.map((u) =>
            u.name === uploadName
              ? { ...u, state: "error", error: message }
              : u,
          ),
        );
      }
    },
    [errorMessage, t],
  );

  const finalizeUpload = useCallback(async () => {
    await loadFirstPage();
    // Drop rows that have been accepted by the backend (they reappear in
    // the real documents list via the post-upload polling grace window).
    // Leave failed rows in place so the operator still sees which files
    // were rejected — they stay visible in the table's status column
    // with the per-file error message until the next batch replaces them.
    setUploadQueue((prev) => {
      const failed = prev.filter((item) => item.state === "error");
      if (failed.length > 0) {
        toast.error(
          t("documents.uploadBatchFailed", { count: failed.length }),
        );
      }
      return failed;
    });
  }, [loadFirstPage, t]);

  const processUploadQueue = useCallback(
    async (candidates: UploadCandidate[]) => {
      if (!activeLibrary || candidates.length === 0) {
        await finalizeUpload();
        return;
      }
      const [candidate, ...remaining] = candidates;
      // We only detect duplicates against documents already visible in
      // the current window. Detecting against the full library would
      // require a name-exact backend lookup — acceptable tradeoff for
      // now because collisions against visible rows are the common case.
      const existing = items.find(
        (d) =>
          normalizeUploadName(d.fileName).toLowerCase() ===
          candidate.name.toLowerCase(),
      );
      if (existing) {
        setDuplicateConflict({
          candidate,
          existingDocId: existing.id,
          remaining,
        });
        return;
      }
      await doUploadFile(candidate);
      await processUploadQueue(remaining);
    },
    [activeLibrary, doUploadFile, finalizeUpload, items],
  );

  const uploadFiles = useCallback(
    async (files: File[]) => {
      if (!activeLibrary) return;
      await processUploadQueue(buildUploadCandidates(files));
    },
    [activeLibrary, processUploadQueue],
  );

  const handleDuplicateReplace = useCallback(async () => {
    if (!duplicateConflict) return;
    const { candidate, existingDocId, remaining } = duplicateConflict;
    setDuplicateConflict(null);
    await doReplaceFile(existingDocId, candidate.file, candidate.name);
    await processUploadQueue(remaining);
  }, [doReplaceFile, duplicateConflict, processUploadQueue]);

  const handleDuplicateAddNew = useCallback(async () => {
    if (!duplicateConflict) return;
    const { candidate, remaining } = duplicateConflict;
    setDuplicateConflict(null);
    await doUploadFile(candidate);
    await processUploadQueue(remaining);
  }, [doUploadFile, duplicateConflict, processUploadQueue]);

  const handleDuplicateSkip = useCallback(async () => {
    if (!duplicateConflict) return;
    const { remaining } = duplicateConflict;
    setDuplicateConflict(null);
    await processUploadQueue(remaining);
  }, [duplicateConflict, processUploadQueue]);

  const handleDrop = useCallback(
    (e: React.DragEvent) => {
      e.preventDefault();
      setDragOver(false);
      const files = Array.from(e.dataTransfer.files);
      void uploadFiles(files);
    },
    [uploadFiles],
  );

  const handleFileSelect = (e: React.ChangeEvent<HTMLInputElement>) => {
    const files = Array.from(e.target.files ?? []);
    void uploadFiles(files);
    e.target.value = "";
  };

  const handleFolderSelect = (e: React.ChangeEvent<HTMLInputElement>) => {
    const files = Array.from(e.target.files ?? []);
    void uploadFiles(files);
    e.target.value = "";
  };

  const handleDelete = useCallback(async () => {
    if (!selectedDoc) return;
    try {
      await documentsApi.delete(selectedDoc.id);
      setDeleteDocOpen(false);
      setSelectedDoc(null);
      updateSearchParamState({ documentId: null });
      await loadFirstPage();
    } catch (err) {
      toast.error(errorMessage(err, t("documents.deleteFailed")));
    }
  }, [errorMessage, loadFirstPage, selectedDoc, t, updateSearchParamState]);

  const handleRetry = useCallback(async () => {
    if (!selectedDoc) return;
    try {
      await documentsApi.reprocess(selectedDoc.id);
      // Activate the whole-list polling grace window before the first
      // refresh so the polling effect already treats this turn as
      // "expect in-flight rows soon" even if the backend has not
      // flipped the status yet — otherwise the single refresh below
      // completes while the row is still `ready`/`failed`, no
      // in-flight marker is observed, and polling never starts.
      setListPollGraceUntil(Date.now() + LIST_POLL_GRACE_MS);
      await loadFirstPage();
      await fetchSelectedDetail(selectedDoc.id);
    } catch (err) {
      toast.error(errorMessage(err, t("documents.reprocessFailed")));
    }
  }, [errorMessage, fetchSelectedDetail, loadFirstPage, selectedDoc, t]);

  const handleReplaceFile = useCallback(async () => {
    if (!selectedDoc || !replaceFile) return;
    setReplaceLoading(true);
    try {
      await documentsApi.replace(selectedDoc.id, replaceFile);
      toast.success(t("documents.replaceFileSuccess"));
      setReplaceFileOpen(false);
      setReplaceFile(null);
      setListPollGraceUntil(Date.now() + LIST_POLL_GRACE_MS);
      await loadFirstPage();
    } catch (err) {
      toast.error(errorMessage(err, t("documents.replaceFileFailed")));
    } finally {
      setReplaceLoading(false);
    }
  }, [errorMessage, loadFirstPage, replaceFile, selectedDoc, t]);

  const handleDocumentEditorSaveRefresh = useCallback(
    async (documentId: string) => {
      await loadFirstPage();
      setInspectorSegments(null);
      setInspectorFacts(null);
      setInspectorLifecycle(null);
      await fetchSelectedDetail(documentId);
    },
    [fetchSelectedDetail, loadFirstPage],
  );

  const documentEditor = useDocumentEditor({
    editAvailability,
    errorMessage,
    onDocumentSaved: handleDocumentEditorSaveRefresh,
    onDocumentSelected: handleSelectDoc,
    selectedDocumentId: selectedDoc?.id ?? null,
    t,
  });

  const handleStartWebIngest = useCallback(async () => {
    if (!activeLibrary || !seedUrl.trim()) return;
    let url = seedUrl.trim();
    if (!/^https?:\/\//i.test(url)) url = `https://${url}`;
    try {
      new URL(url);
    } catch {
      toast.error(t("documents.invalidUrl"));
      return;
    }
    setWebIngestLoading(true);
    try {
      await documentsApi.createWebIngestRun({
        libraryId: activeLibrary.id,
        seedUrl: url,
        mode: crawlMode,
        boundaryPolicy,
        maxDepth: parseInt(maxDepth, 10),
        maxPages: parseInt(maxPages, 10),
      });
      toast.success(t("documents.webIngestStarted"));
      setAddLinkOpen(false);
      setSeedUrl("");
      setCrawlMode("recursive_crawl");
      setBoundaryPolicy("same_host");
      setMaxDepth("3");
      setMaxPages("30");
      await loadFirstPage();
    } catch (err) {
      toast.error(errorMessage(err, t("documents.webIngestFailed")));
    } finally {
      setWebIngestLoading(false);
    }
  }, [
    activeLibrary,
    boundaryPolicy,
    crawlMode,
    errorMessage,
    loadFirstPage,
    maxDepth,
    maxPages,
    seedUrl,
    t,
  ]);

  // ----- Bulk selection -----

  const toggleSelection = (id: string) => {
    setSelectedIds((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  const clearSelection = () => {
    setSelectedIds(new Set());
    setSelectionMode(false);
    setExpandingSelection(false);
  };

  const selectedCount = selectedIds.size;

  /**
   * Expands the current selection to cover every row matching the active
   * filter, not just the visible page. Walks the server's keyset pagination
   * with the already-tuned page size, accumulating the id column only.
   * At up to 1000 rows per fetch this is a handful of round-trips even
   * for the 2000+ pending-doc libraries the header count shows up on.
   * The page's filter state (search, status, sort) is reused verbatim so
   * the fetch returns exactly the same set the header-count promised.
   */
  const selectAllMatching = useCallback(async () => {
    if (!activeLibrary || expandingSelection) return;
    setExpandingSelection(true);
    try {
      const { sortBy, sortOrder } = splitSortValue(sortValue);
      const collected = new Set<string>(selectedIds);
      let cursor: string | null | undefined = undefined;
      // Safety cap — if something goes sideways we never want to burn
      // more than a handful of seconds of fetches. 100k matches 1/10 of
      // the largest library the backend will even let us restore into
      // via snapshot.
      const hardCap = 100_000;
      while (collected.size < hardCap) {
        const page: DocumentListPageResponse = await documentsApi.list({
          libraryId: activeLibrary.id,
          limit: pageSize,
          cursor: cursor ?? undefined,
          search: debouncedSearch || undefined,
          sortBy,
          sortOrder,
          includeTotal: false,
          status:
            statusBackendFilter.length > 0 ? statusBackendFilter : undefined,
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
    activeLibrary,
    debouncedSearch,
    errorMessage,
    expandingSelection,
    pageSize,
    selectedIds,
    sortValue,
    statusBackendFilter,
    t,
  ]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && selectionMode) clearSelection();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [selectionMode]);

  const handleBulkDelete = async () => {
    if (!confirm(t("documents.confirmBulkDelete", { count: selectedCount })))
      return;
    try {
      await documentsApi.batchDelete(Array.from(selectedIds));
      toast.success(t("documents.bulkDeleteSuccess", { count: selectedCount }));
      clearSelection();
      await loadFirstPage();
    } catch {
      toast.error(t("documents.bulkDeleteFailed"));
    }
  };

  const handleBulkCancel = async () => {
    try {
      await documentsApi.batchCancel(Array.from(selectedIds));
      toast.success(t("documents.bulkCancelSuccess", { count: selectedCount }));
      clearSelection();
      await loadFirstPage();
    } catch {
      toast.error(t("documents.bulkCancelFailed"));
    }
  };

  // Canonical async batch rerun flow.
  //
  // 1. POST /content/documents/batch-reprocess -> 202 Accepted with a parent
  //    `batchOperationId`. All per-document child mutations are linked back
  //    to that parent so progress can be aggregated in a single query.
  // 2. Poll GET /v1/ops/operations/{id} with a modest backoff. The poll
  //    returns the parent row + aggregated child counts.
  // 3. Stop once the parent enters a terminal state (ready / failed /
  //    canceled / superseded); render the final toast and reload the list.
  //
  // The list is NOT reloaded mid-flight — children flip the document status
  // individually via the normal list path once the component remounts or
  // the user navigates, which matches how single-document reprocess behaves.
  const handleBulkReprocess = async () => {
    const ids = Array.from(selectedIds);
    if (ids.length === 0) return;
    try {
      const accepted = await documentsApi.batchReprocess(ids);
      // Keep the list auto-refreshing while the bulk rerun drains,
      // same pattern as `handleRetry` — the list polling effect
      // walks each affected row through queued/processing/ready
      // automatically until all terminal.
      setListPollGraceUntil(Date.now() + LIST_POLL_GRACE_MS);
      clearSelection();
      setBulkRerun({
        operationId: accepted.batchOperationId,
        total: accepted.total,
        completed: 0,
        failed: 0,
        inFlight: accepted.total,
        status: "processing",
      });
      bulkRerunAbortedRef.current = false;
      pollBulkRerunProgress(accepted.batchOperationId, 1500);
    } catch {
      toast.error(t("documents.bulkReprocessFailed"));
    }
  };

  const pollBulkRerunProgress = useCallback(
    (operationId: string, delayMs: number) => {
      if (bulkRerunPollRef.current) {
        clearTimeout(bulkRerunPollRef.current);
      }
      bulkRerunPollRef.current = setTimeout(async () => {
        if (bulkRerunAbortedRef.current) return;
        try {
          const detail = await opsApi.getAsyncOperation(operationId);
          setBulkRerun({
            operationId,
            total: detail.progress.total || 0,
            completed: detail.progress.completed,
            failed: detail.progress.failed,
            inFlight: detail.progress.inFlight,
            status: detail.status,
          });
          if (ASYNC_OPERATION_TERMINAL_STATES.has(detail.status)) {
            bulkRerunPollRef.current = null;
            if (detail.status === "ready") {
              toast.success(
                t("documents.bulkReprocessSuccess", {
                  count: detail.progress.completed,
                }),
              );
            } else if (detail.progress.completed > 0) {
              toast.warning(
                t("documents.bulkReprocessPartial", {
                  ok: detail.progress.completed,
                  failed: detail.progress.failed,
                }),
              );
            } else {
              toast.error(
                t("documents.bulkReprocessAllFailed", {
                  count: detail.progress.failed,
                }),
              );
            }
            await loadFirstPage();
            // Keep the banner visible for a brief moment so users see the
            // terminal state, then clear it.
            setTimeout(() => {
              if (!bulkRerunAbortedRef.current) setBulkRerun(null);
            }, 4000);
            return;
          }
          // Gentle backoff: 1.5s -> 2s -> 3s -> 5s ceiling.
          const nextDelay = Math.min(Math.round(delayMs * 1.35), 5000);
          pollBulkRerunProgress(operationId, nextDelay);
        } catch {
          // Transient poll failure — keep trying, but surface a single toast
          // and back off to the ceiling so we do not hammer the backend.
          const nextDelay = Math.min(Math.round(delayMs * 1.5), 5000);
          pollBulkRerunProgress(operationId, nextDelay);
        }
      }, delayMs);
    },
    [loadFirstPage, t],
  );

  // ----- Render helpers -----

  // Controlled clock so rows that show "processing elapsed" update at a
  // predictable cadence instead of reading `Date.now()` on every single
  // render. The old `const processingClockMs = Date.now()` was the
  // dominant flicker source: every poll, selection change, debounce
  // tick, etc. re-read the clock, invalidated `displayedItems`'
  // `useMemo`, and forced every row's "time" cell to paint a new
  // integer. The clock now advances only when at least one doc is
  // queued/processing (idle libraries never tick) and only once per
  // second.
  const [processingClockMs, setProcessingClockMs] = useState<number>(() =>
    Date.now(),
  );
  const hasInFlightDocs = useMemo(
    () =>
      items.some(
        (doc) => doc.status === "queued" || doc.status === "processing",
      ),
    [items],
  );
  useEffect(() => {
    if (!hasInFlightDocs) return undefined;
    const id = window.setInterval(() => setProcessingClockMs(Date.now()), 1000);
    return () => window.clearInterval(id);
  }, [hasInFlightDocs]);
  const totalCountForHeader = totalCount ?? items.length;
  const totalCost = libraryTotalCost ?? 0;

  const { sortBy, sortOrder } = splitSortValue(sortValue);
  const sortIcon =
    sortOrder === "asc" ? (
      <ArrowUp className="h-3 w-3" />
    ) : (
      <ArrowDown className="h-3 w-3" />
    );

  const onSortChange = (value: SortValue) => {
    updateSearchParamState({
      sort: value === "uploaded_at:desc" ? null : value,
      documentId: null,
    });
  };

  const toggleSortDirection = (target: DocumentListSortKey) => {
    if (sortBy !== target) {
      onSortChange(`${target}:${sortOrder}`);
      return;
    }
    onSortChange(`${target}:${sortOrder === "asc" ? "desc" : "asc"}`);
  };

  /**
   * Page-local sort state for columns that the backend can't cheaply push
   * down (cost / processing time / finished timestamp). Those columns
   * would require a heavy billing aggregate join on every page fetch —
   * the canonical compromise is to reorder the currently loaded page
   * client-side. If the user wants sort by one of these columns across
   * the full library, they narrow via a status filter first.
   */
  type LocalSortKey = "cost" | "time" | "finished";
  const [localSort, setLocalSort] = useState<{
    key: LocalSortKey;
    direction: "asc" | "desc";
  } | null>(null);

  const toggleLocalSort = (key: LocalSortKey) => {
    setLocalSort((prev) =>
      prev && prev.key === key
        ? { key, direction: prev.direction === "asc" ? "desc" : "asc" }
        : { key, direction: "desc" },
    );
  };

  const displayedItems = useMemo(() => {
    if (!localSort) return items;
    const direction = localSort.direction === "asc" ? 1 : -1;
    const score = (doc: DocumentItem): number => {
      switch (localSort.key) {
        case "cost":
          return doc.cost ?? -Infinity;
        case "time": {
          const duration = getDocumentProcessingDurationMs(
            doc,
            processingClockMs,
          );
          return duration ?? -Infinity;
        }
        case "finished":
          return doc.processingFinishedAt
            ? Date.parse(doc.processingFinishedAt)
            : -Infinity;
      }
    };
    return [...items].sort((left, right) => {
      const lhs = score(left);
      const rhs = score(right);
      if (lhs === rhs) return 0;
      return lhs < rhs ? -1 * direction : 1 * direction;
    });
  }, [items, localSort, processingClockMs]);

  const localSortIcon =
    localSort?.direction === "asc" ? (
      <ArrowUp className="h-3 w-3" />
    ) : (
      <ArrowDown className="h-3 w-3" />
    );

  // Upload rows the table should render as virtual entries above the real
  // documents. An upload disappears from this list the moment the backend
  // accepts it (state === "done") because the polling grace window will
  // surface the matching real row; failures stay visible with their error
  // message in the status column until the operator starts a new batch.
  const pendingUploads = useMemo(
    () => uploadQueue.filter((item) => item.state !== "done"),
    [uploadQueue],
  );

  if (!activeLibrary) {
    return (
      <div className="flex-1 flex flex-col">
        <div className="page-header">
          <h1 className="text-lg font-bold tracking-tight">
            {t("documents.title")}
          </h1>
        </div>
        <div className="empty-state flex-1">
          <div className="w-14 h-14 rounded-2xl bg-muted flex items-center justify-center mb-4">
            <FileText className="h-7 w-7 text-muted-foreground" />
          </div>
          <h2 className="text-base font-bold tracking-tight">
            {t("documents.noLibrary")}
          </h2>
          <p className="text-sm text-muted-foreground mt-2">
            {t("documents.noLibraryDesc")}
          </p>
        </div>
      </div>
    );
  }

  return (
    <div className="flex-1 flex flex-col overflow-hidden">
      <DocumentsPageHeader
        activeLibraryName={activeLibrary.name}
        activeTab={activeTab}
        documentsCount={totalCountForHeader}
        fileInputRef={fileInputRef}
        folderInputRef={folderInputRef}
        handleFileSelect={handleFileSelect}
        handleFolderSelect={handleFolderSelect}
        setActiveTab={setActiveTab}
        setAddLinkOpen={setAddLinkOpen}
        setBoundaryPolicy={setBoundaryPolicy}
        setCrawlMode={setCrawlMode}
        setMaxDepth={setMaxDepth}
        setMaxPages={setMaxPages}
        setSeedUrl={setSeedUrl}
        t={t}
        webRunsCount={webRuns.length}
      />

      {activeTab === "documents" && (
        <div className="px-6 py-3 border-b flex flex-wrap items-center gap-3 bg-surface-sunken/50">
          <div className="relative flex-1 min-w-[200px] max-w-md">
            <Search className="absolute left-3 top-1/2 -translate-y-1/2 h-3.5 w-3.5 text-muted-foreground" />
            <Input
              className="h-9 pl-9 text-sm"
              placeholder={t("documents.searchPlaceholder")}
              value={searchQuery}
              onChange={(e) =>
                updateSearchParamState({
                  q: e.target.value || null,
                  documentId: null,
                })
              }
            />
          </div>
          {/* Canonical filter pills — one rounded-xl container with an
              inner segmented group, 0.2.3 look: icon + label + count
              badge. Counts come from the aggregate the backend already
              computes alongside `includeTotal`, so the pill badges stay
              live as the operator searches or deletes. Clicking a pill
              sets `?status=<bucket>` which the effect above translates
              into the canonical backend filter. */}
          <div className="flex flex-wrap gap-0.5 p-1 bg-muted rounded-xl border border-border/50">
            {[
              {
                key: "all" as const,
                label: t("documents.all"),
                count: statusCounts?.total ?? null,
                icon: null,
              },
              {
                key: "ready" as const,
                label: t("documents.statusReady"),
                count: statusCounts?.ready ?? null,
                icon: <CheckCircle2 className="h-3 w-3 text-status-ready" />,
              },
              {
                key: "processing" as const,
                label: t("documents.statusProcessing"),
                count: statusCounts?.processing ?? null,
                icon: <Clock className="h-3 w-3 text-status-processing" />,
              },
              {
                key: "queued" as const,
                label: t("documents.statusQueued"),
                count: statusCounts?.queued ?? null,
                icon: <Hourglass className="h-3 w-3 text-status-queued" />,
              },
              {
                key: "failed" as const,
                label: t("documents.statusFailed"),
                count: statusCounts?.failed ?? null,
                icon: <XCircle className="h-3 w-3 text-status-failed" />,
              },
              {
                key: "canceled" as const,
                label: t("documents.statusCanceled"),
                count: statusCounts?.canceled ?? null,
                icon: <Ban className="h-3 w-3 text-status-stalled" />,
              },
            ]
              .map((bucket) => ({
                ...bucket,
                active: statusBucket === bucket.key,
              }))
              .map((bucket) => (
                <button
                  key={bucket.key}
                  type="button"
                  className={`px-3 py-1.5 text-xs rounded-[9px] transition-all duration-200 font-medium flex items-center gap-1.5 ${
                    bucket.active
                      ? "bg-card shadow-soft font-semibold text-foreground"
                      : "text-muted-foreground hover:text-foreground"
                  }`}
                  onClick={() =>
                    updateSearchParamState({
                      status: bucket.key === "all" ? null : bucket.key,
                      documentId: null,
                    })
                  }
                >
                  {bucket.icon}
                  {bucket.label}
                  {bucket.count != null && bucket.count > 0 && (
                    <span className="tabular-nums text-[10px] opacity-70">
                      {bucket.count}
                    </span>
                  )}
                </button>
              ))}
          </div>
          {totalCost > 0 && (
            <span className="text-xs text-muted-foreground ml-auto mr-2">
              {t("documents.totalCost")}:{" "}
              <span className="font-bold tabular-nums">
                ${totalCost.toFixed(3)}
              </span>
            </span>
          )}
          <Button
            size="sm"
            variant={selectionMode ? "default" : "outline"}
            className={`${totalCost > 0 ? "" : "ml-auto"} h-8 text-xs`}
            onClick={() =>
              selectionMode ? clearSelection() : setSelectionMode(true)
            }
          >
            <CheckSquare className="h-3.5 w-3.5 mr-1.5" />
            {selectionMode
              ? t("documents.cancelSelection")
              : t("documents.select")}
          </Button>
        </div>
      )}

      {/* Main area */}
      <div className="flex-1 flex overflow-hidden">
        <div
          className={`flex-1 min-w-0 overflow-hidden ${
            activeTab === "documents" && dragOver
              ? "ring-2 ring-primary ring-inset bg-primary/5"
              : ""
          }`}
          onDragOver={
            activeTab === "documents"
              ? (e) => {
                  e.preventDefault();
                  setDragOver(true);
                }
              : undefined
          }
          onDragLeave={
            activeTab === "documents" ? () => setDragOver(false) : undefined
          }
          onDrop={activeTab === "documents" ? handleDrop : undefined}
        >
          {activeTab === "documents" ? (
            <>
              {bulkRerun && (
                <div className="mx-4 mt-4">
                  <BulkRerunProgressBanner
                    bulkRerun={bulkRerun}
                    onDismiss={() => setBulkRerun(null)}
                    t={t}
                  />
                </div>
              )}
              {/* Select-all-matching banner. Shows when selection mode is on,
                  every visible row is already selected, AND the filter
                  matches more rows than the visible page. Clicking the
                  button walks every page of matching IDs and stuffs them
                  all into the selection set, so the next batch action
                  covers the whole filtered set — not just the 200-1000
                  rows currently rendered. This fixes the "Выбрать все"
                  surprise where cancel/delete would only hit the visible
                  page. */}
              {selectionMode &&
                items.length > 0 &&
                items.every((d) => selectedIds.has(d.id)) &&
                filteredTotal != null &&
                filteredTotal > items.length &&
                selectedIds.size < filteredTotal && (
                  <div className="mx-4 mt-4 rounded-xl border border-primary/20 bg-primary/5 px-4 py-2.5 text-sm flex items-center justify-between gap-3">
                    <span>
                      {t("documents.selectAllBannerSelected", {
                        count: selectedIds.size,
                      })}
                    </span>
                    <Button
                      size="sm"
                      variant="default"
                      className="h-7 text-xs shrink-0"
                      disabled={expandingSelection}
                      onClick={() => void selectAllMatching()}
                    >
                      {expandingSelection ? (
                        <>
                          <Loader2 className="h-3 w-3 mr-1.5 animate-spin" />
                          {t("documents.selectAllBannerExpanding")}
                        </>
                      ) : (
                        t("documents.selectAllBannerAction", {
                          total: filteredTotal,
                        })
                      )}
                    </Button>
                  </div>
                )}
              {dragOver && (
                <div className="absolute inset-0 z-10 flex items-center justify-center pointer-events-none">
                  <div className="p-8 rounded-2xl border-2 border-dashed border-primary bg-card shadow-elevated">
                    <Upload className="h-8 w-8 text-primary mx-auto mb-3" />
                    <p className="text-sm font-bold">
                      {t("documents.dropToUpload")}
                    </p>
                  </div>
                </div>
              )}

              {isLoading && items.length === 0 ? (
                <div className="empty-state py-20">
                  <Loader2 className="h-7 w-7 animate-spin text-primary mb-4" />
                  <h2 className="text-base font-bold tracking-tight">
                    {t("documents.loadingDocs")}
                  </h2>
                </div>
              ) : loadError && items.length === 0 ? (
                <div className="empty-state py-20">
                  <div className="w-14 h-14 rounded-2xl bg-destructive/10 flex items-center justify-center mb-4">
                    <XCircle className="h-7 w-7 text-destructive" />
                  </div>
                  <h2 className="text-base font-bold tracking-tight">
                    {t("documents.failedToLoad")}
                  </h2>
                  <p className="text-sm text-muted-foreground mt-2">
                    {loadError}
                  </p>
                  <Button
                    size="sm"
                    variant="outline"
                    className="mt-4"
                    onClick={() => void loadFirstPage()}
                  >
                    <RotateCw className="h-3.5 w-3.5 mr-1.5" />{" "}
                    {t("documents.retry")}
                  </Button>
                </div>
              ) : items.length === 0 && pendingUploads.length === 0 ? (
                <div className="empty-state py-20">
                  <div className="w-14 h-14 rounded-2xl bg-muted flex items-center justify-center mb-4">
                    <FileText className="h-7 w-7 text-muted-foreground" />
                  </div>
                  <h2 className="text-base font-bold tracking-tight">
                    {searchQuery
                      ? t("documents.noMatchingDocs")
                      : t("documents.noDocs")}
                  </h2>
                  <p className="text-sm text-muted-foreground mt-2">
                    {searchQuery
                      ? t("documents.noMatchingDocsDesc")
                      : t("documents.noDocsDesc")}
                  </p>
                </div>
              ) : (
                <div className="flex h-full min-h-0 flex-col">
                  <div className="min-h-0 flex-1 overflow-auto">
                    <table className="w-full text-sm">
                      <thead
                        className="sticky top-0 z-10"
                        style={{
                          background:
                            "linear-gradient(180deg, hsl(var(--card)), hsl(var(--card) / 0.95))",
                          backdropFilter: "blur(8px)",
                        }}
                      >
                        <tr className="border-b text-left">
                          {selectionMode && (
                            <th className="px-4 py-3 w-10">
                              <input
                                type="checkbox"
                                checked={
                                  items.length > 0 &&
                                  items.every((d) => selectedIds.has(d.id))
                                }
                                onChange={() => {
                                  const allSelected =
                                    items.length > 0 &&
                                    items.every((d) => selectedIds.has(d.id));
                                  setSelectedIds((prev) => {
                                    const next = new Set(prev);
                                    for (const doc of items) {
                                      if (allSelected) next.delete(doc.id);
                                      else next.add(doc.id);
                                    }
                                    return next;
                                  });
                                }}
                                className="h-4 w-4 rounded border-gray-300"
                              />
                            </th>
                          )}
                          <th className="px-4 py-3 section-label">
                            <button
                              className="flex items-center gap-1 hover:text-foreground transition-colors"
                              onClick={() => toggleSortDirection("file_name")}
                            >
                              {t("documents.name")}
                              {sortBy === "file_name" && sortIcon}
                            </button>
                          </th>
                          <th className="px-4 py-3 section-label">
                            <button
                              className="flex items-center gap-1 hover:text-foreground transition-colors"
                              onClick={() => toggleSortDirection("file_type")}
                            >
                              {t("documents.type")}
                              {sortBy === "file_type" && sortIcon}
                            </button>
                          </th>
                          <th className="px-4 py-3 section-label">
                            <button
                              className="flex items-center gap-1 hover:text-foreground transition-colors"
                              onClick={() => toggleSortDirection("file_size")}
                            >
                              {t("documents.size")}
                              {sortBy === "file_size" && sortIcon}
                            </button>
                          </th>
                          <th className="px-4 py-3 section-label">
                            <button
                              className="flex items-center gap-1 hover:text-foreground transition-colors"
                              onClick={() => toggleSortDirection("uploaded_at")}
                            >
                              {t("documents.uploaded")}
                              {sortBy === "uploaded_at" && sortIcon}
                            </button>
                          </th>
                          <th className="px-4 py-3 section-label">
                            <button
                              className="flex items-center gap-1 hover:text-foreground transition-colors"
                              title={t("documents.pageLocalSortHint")}
                              onClick={() => toggleLocalSort("cost")}
                            >
                              {t("documents.cost")}
                              {localSort?.key === "cost" && localSortIcon}
                            </button>
                          </th>
                          <th className="px-4 py-3 section-label">
                            <button
                              className="flex items-center gap-1 hover:text-foreground transition-colors"
                              title={t("documents.pageLocalSortHint")}
                              onClick={() => toggleLocalSort("time")}
                            >
                              {t("documents.pipelineTime")}
                              {localSort?.key === "time" && localSortIcon}
                            </button>
                          </th>
                          <th className="px-4 py-3 section-label">
                            <button
                              className="flex items-center gap-1 hover:text-foreground transition-colors"
                              title={t("documents.pageLocalSortHint")}
                              onClick={() => toggleLocalSort("finished")}
                            >
                              {t("documents.finished")}
                              {localSort?.key === "finished" && localSortIcon}
                            </button>
                          </th>
                          <th className="px-4 py-3 section-label">
                            <button
                              className="flex items-center gap-1 hover:text-foreground transition-colors"
                              onClick={() => toggleSortDirection("status")}
                            >
                              {t("documents.status")}
                              {sortBy === "status" && sortIcon}
                            </button>
                          </th>
                        </tr>
                      </thead>
                      <tbody>
                        {pendingUploads.map((upload) => {
                          const isError = upload.state === "error";
                          const fileNameLabel = compactText(upload.name, 28);
                          return (
                            <tr
                              key={`upload-${upload.name}`}
                              className="border-b opacity-80"
                            >
                              {selectionMode && (
                                <td className="px-4 py-3.5 w-10" />
                              )}
                              <td className="px-4 py-3.5">
                                <div className="flex items-center gap-3">
                                  <div className="w-8 h-8 rounded-xl flex items-center justify-center shrink-0 bg-surface-sunken">
                                    <File className="h-3.5 w-3.5 text-muted-foreground" />
                                  </div>
                                  <div className="min-w-0">
                                    <span
                                      className="truncate block max-w-[240px] font-semibold"
                                      title={fileNameLabel.fullText}
                                    >
                                      {fileNameLabel.text}
                                    </span>
                                  </div>
                                </div>
                              </td>
                              {/* Fills type + size + uploaded + cost + pipelineTime + finished — six middle columns between name and status. */}
                              <td
                                className="px-4 py-3.5 text-muted-foreground text-[10px]"
                                colSpan={6}
                              />
                              <td className="px-4 py-3.5">
                                {isError ? (
                                  <span
                                    className="inline-flex items-center gap-1.5 text-xs text-status-failed"
                                    title={upload.error}
                                  >
                                    <XCircle className="h-3 w-3 shrink-0" />
                                    <span className="truncate max-w-[260px]">
                                      {upload.error ??
                                        t("documents.uploadFailed")}
                                    </span>
                                  </span>
                                ) : (
                                  <span className="inline-flex items-center gap-1.5 text-xs text-muted-foreground">
                                    <Loader2 className="h-3 w-3 animate-spin text-primary" />
                                    {t("documents.uploading")}
                                  </span>
                                )}
                              </td>
                            </tr>
                          );
                        })}
                        {displayedItems.map((doc) => {
                          const rc = statusBadgeConfig[doc.status];
                          const isWebPage = isWebPageDocument(
                            doc.sourceKind,
                            doc.sourceUri,
                            doc.fileName,
                          );
                          const typeLabel = formatDocumentTypeLabel(
                            doc.fileType,
                            doc.sourceKind,
                            t,
                            {
                              sourceUri: doc.sourceUri,
                              fileName: doc.fileName,
                            },
                          );
                          const fileNameLabel = compactText(doc.fileName, 28);
                          const sourceUriLabel = compactText(doc.sourceUri, 36);
                          const processingDurationMs =
                            getDocumentProcessingDurationMs(
                              doc,
                              processingClockMs,
                            );
                          return (
                            <tr
                              key={doc.id}
                              className={`border-b cursor-pointer transition-all duration-150 ${
                                selectedIds.has(doc.id)
                                  ? "bg-primary/10"
                                  : selectedDoc?.id === doc.id
                                    ? "bg-primary/5 border-l-2 border-l-primary"
                                    : "hover:bg-accent/30"
                              }`}
                              onClick={() =>
                                selectionMode
                                  ? toggleSelection(doc.id)
                                  : handleSelectDoc(doc)
                              }
                            >
                              {selectionMode && (
                                <td className="px-4 py-3.5 w-10">
                                  <input
                                    type="checkbox"
                                    checked={selectedIds.has(doc.id)}
                                    onChange={(e) => {
                                      e.stopPropagation();
                                      toggleSelection(doc.id);
                                    }}
                                    onClick={(e) => e.stopPropagation()}
                                    className="h-4 w-4 rounded border-gray-300"
                                  />
                                </td>
                              )}
                              <td className="px-4 py-3.5">
                                <div className="flex items-center gap-3">
                                  <div
                                    className={`w-8 h-8 rounded-xl flex items-center justify-center shrink-0 ${
                                      isWebPage
                                        ? "bg-blue-100 dark:bg-blue-900/30"
                                        : "bg-surface-sunken"
                                    }`}
                                  >
                                    {isWebPage ? (
                                      <Globe className="h-3.5 w-3.5 text-blue-600 dark:text-blue-400" />
                                    ) : (
                                      <File className="h-3.5 w-3.5 text-muted-foreground" />
                                    )}
                                  </div>
                                  <div className="min-w-0">
                                    <span
                                      className="truncate block max-w-[240px] font-semibold"
                                      title={fileNameLabel.fullText}
                                    >
                                      {fileNameLabel.text}
                                    </span>
                                    {isWebPage &&
                                      doc.sourceUri &&
                                      doc.sourceUri !== doc.fileName && (
                                        <span
                                          className="truncate block max-w-[240px] text-[10px] text-muted-foreground"
                                          title={sourceUriLabel.fullText}
                                        >
                                          {sourceUriLabel.text}
                                        </span>
                                      )}
                                  </div>
                                </div>
                              </td>
                              <td
                                className={`px-4 py-3.5 text-muted-foreground text-[10px] font-bold tracking-widest ${
                                  isWebPage ? "" : "uppercase"
                                }`}
                                title={typeLabel}
                              >
                                {typeLabel}
                              </td>
                              <td className="px-4 py-3.5 text-muted-foreground tabular-nums text-xs">
                                {formatSize(doc.fileSize)}
                              </td>
                              <td className="px-4 py-3.5 text-muted-foreground text-xs">
                                {formatDate(doc.uploadedAt, locale)}
                              </td>
                              <td className="px-4 py-3.5 text-muted-foreground tabular-nums text-xs">
                                {doc.cost != null
                                  ? `$${doc.cost.toFixed(3)}`
                                  : "\u2014"}
                              </td>
                              <td className="px-4 py-3.5 text-muted-foreground tabular-nums text-xs">
                                {processingDurationMs != null
                                  ? `${Math.floor(processingDurationMs / 1000)}s`
                                  : "\u2014"}
                              </td>
                              <td className="px-4 py-3.5 text-muted-foreground text-xs">
                                {doc.processingFinishedAt
                                  ? formatDate(doc.processingFinishedAt, locale)
                                  : "\u2014"}
                              </td>
                              <td className="px-4 py-3.5">
                                <span
                                  className={`status-badge ${rc.cls}`}
                                  title={doc.statusReason}
                                >
                                  {rc.label}
                                </span>
                              </td>
                            </tr>
                          );
                        })}
                      </tbody>
                    </table>
                  </div>
                  {items.length > 0 && (
                    <div className="shrink-0 border-t bg-background/95 px-4 py-3 shadow-[0_-8px_24px_hsl(var(--background)/0.9)] backdrop-blur supports-[backdrop-filter]:bg-background/85">
                      <div className="flex flex-wrap items-center gap-3">
                        <span className="text-xs font-medium text-muted-foreground tabular-nums">
                          {t("documents.paginationSummary", {
                            from: visibleRangeStart,
                            to: visibleRangeEnd,
                            total: filteredTotal ?? items.length,
                          })}
                        </span>

                        <div className="flex items-center gap-2 md:ml-auto">
                          <span className="text-xs text-muted-foreground">
                            {t("documents.pageSize")}
                          </span>
                          <Select
                            value={String(pageSize)}
                            onValueChange={(value) =>
                              updateSearchParamState({
                                pageSize:
                                  value === String(DEFAULT_PAGE_SIZE)
                                    ? null
                                    : value,
                                documentId: null,
                              })
                            }
                          >
                            <SelectTrigger className="h-8 w-[92px] text-xs">
                              <SelectValue />
                            </SelectTrigger>
                            <SelectContent>
                              {PAGE_SIZE_OPTIONS.map((option) => (
                                <SelectItem key={option} value={String(option)}>
                                  {option}
                                </SelectItem>
                              ))}
                            </SelectContent>
                          </Select>
                        </div>

                        <div className="flex items-center gap-2">
                          <Button
                            variant="outline"
                            size="sm"
                            className="h-8 text-xs"
                            disabled={!canGoPrevious}
                            onClick={goToPreviousPage}
                          >
                            {t("documents.previous")}
                          </Button>
                          <span className="min-w-[112px] text-center text-xs font-medium text-muted-foreground tabular-nums">
                            {totalPages != null
                              ? t("documents.pageLabel", {
                                  page: currentPageNumber,
                                  total: totalPages,
                                })
                              : t("documents.pageLabelSimple", {
                                  page: currentPageNumber,
                                })}
                          </span>
                          <Button
                            variant="outline"
                            size="sm"
                            className="h-8 text-xs"
                            disabled={!canGoNext}
                            onClick={goToNextPage}
                          >
                            {t("documents.next")}
                          </Button>
                        </div>
                      </div>
                    </div>
                  )}
                </div>
              )}
            </>
          ) : (
            <WebRunsPanel
              t={t}
              webRuns={webRuns}
              onReuseRun={(run) => {
                setSeedUrl(run.seedUrl);
                setCrawlMode(run.mode);
                setBoundaryPolicy(run.boundaryPolicy || "same_host");
                setMaxDepth(String(run.maxDepth ?? 3));
                setMaxPages(String(run.maxPages ?? 100));
                setAddLinkOpen(true);
              }}
            />
          )}
        </div>

        {selectedDoc && (
          <DocumentsInspectorPanel
            canEdit={editAvailability(selectedDoc).enabled}
            editDisabledReason={editAvailability(selectedDoc).reason}
            locale={locale}
            t={t}
            inspectorFacts={inspectorFacts}
            inspectorSegments={inspectorSegments}
            lifecycle={inspectorLifecycle}
            selectedDoc={selectedDoc}
            selectionMode={selectionMode}
            setDeleteDocOpen={setDeleteDocOpen}
            setReplaceFileOpen={setReplaceFileOpen}
            updateSearchParamState={updateSearchParamState}
            onEdit={() => void documentEditor.openEditor(selectedDoc)}
            onRetry={handleRetry}
          />
        )}
      </div>

      <DocumentsOverlays
        activeTab={activeTab}
        addLinkOpen={addLinkOpen}
        boundaryPolicy={boundaryPolicy}
        clearSelection={clearSelection}
        crawlMode={crawlMode}
        deleteDocOpen={deleteDocOpen}
        handleBulkCancel={handleBulkCancel}
        handleBulkDelete={handleBulkDelete}
        handleBulkReprocess={handleBulkReprocess}
        handleDelete={handleDelete}
        handleReplaceFile={handleReplaceFile}
        handleStartWebIngest={handleStartWebIngest}
        maxDepth={maxDepth}
        maxPages={maxPages}
        replaceFile={replaceFile}
        replaceFileInputRef={replaceFileInputRef}
        replaceFileOpen={replaceFileOpen}
        replaceLoading={replaceLoading}
        seedUrl={seedUrl}
        selectedCount={selectedCount}
        selectedDoc={selectedDoc}
        setAddLinkOpen={setAddLinkOpen}
        setBoundaryPolicy={setBoundaryPolicy}
        setCrawlMode={setCrawlMode}
        setDeleteDocOpen={setDeleteDocOpen}
        setMaxDepth={setMaxDepth}
        setMaxPages={setMaxPages}
        setReplaceFile={setReplaceFile}
        setReplaceFileOpen={setReplaceFileOpen}
        setSeedUrl={setSeedUrl}
        t={t}
        webIngestLoading={webIngestLoading}
      />

      <Dialog
        open={Boolean(duplicateConflict)}
        onOpenChange={(open) => {
          if (!open) void handleDuplicateSkip();
        }}
      >
        <DialogContent className="sm:max-w-md">
          <DialogHeader>
            <DialogTitle>{t("documents.duplicateTitle")}</DialogTitle>
            <DialogDescription className="break-all">
              {t("documents.duplicateDescription", {
                name: duplicateConflict?.candidate.name ?? "",
              })}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter className="flex-col gap-2 sm:flex-row">
            <Button variant="default" onClick={handleDuplicateReplace}>
              <RotateCw className="mr-2 h-3.5 w-3.5" />{" "}
              {t("documents.duplicateReplace")}
            </Button>
            <Button variant="outline" onClick={handleDuplicateAddNew}>
              <Upload className="mr-2 h-3.5 w-3.5" />{" "}
              {t("documents.duplicateAddNew")}
            </Button>
            <Button variant="ghost" onClick={handleDuplicateSkip}>
              {t("documents.duplicateSkip")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {documentEditor.editorDocument && (
        <DocumentEditorShell
          documentName={documentEditor.editorDocument.fileName}
          error={documentEditor.editorError}
          loading={documentEditor.editorLoading}
          markdown={documentEditor.editorMarkdown}
          onOpenChange={documentEditor.handleEditorOpenChange}
          onSave={documentEditor.saveEditor}
          open={documentEditor.editorOpen}
          saving={documentEditor.editorSaving}
          sourceFormat={documentEditor.editorDocument.fileType}
          t={t}
        />
      )}
    </div>
  );
}
