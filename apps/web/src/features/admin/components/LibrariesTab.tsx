import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { JSX, KeyboardEventHandler, MouseEventHandler, ReactNode } from "react";
import type { TFunction } from "i18next";
import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router-dom";
import { useQueries, useQuery, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import {
  ArrowDown,
  ArrowUp,
  Ban,
  BookOpen,
  CheckCircle2,
  CheckSquare,
  Database,
  Download,
  ExternalLink,
  FileText,
  Filter,
  HelpCircle,
  Loader2,
  RotateCw,
  Search,
  Settings2,
  Trash2,
  XCircle,
} from "lucide-react";

import {
  ASYNC_OPERATION_TERMINAL_STATES,
  Catalog,
  Ops,
  librarySnapshotApi,
  queries,
  unwrap,
} from "@/shared/api";
import type {
  CatalogLibraryResponse,
  CatalogWorkspaceResponse,
  LibraryCostSummary,
  WorkspaceCostSummary,
} from "@/shared/api/generated";
import { TablePaginationFooter } from "@/shared/components/TablePaginationFooter";
import { Button } from "@/shared/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/shared/components/ui/dialog";
import { Input } from "@/shared/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/shared/components/ui/select";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/shared/components/ui/tooltip";
import { DataState } from "@/shared/components/DataState";
import { useApp } from "@/shared/contexts/app-context";
import { errorMessage } from "@/shared/lib/errorMessage";

const PAGE_SIZE_OPTIONS = [50, 100, 250, 1000] as const;
const DELETE_POLL_INTERVAL_MS = 2_000;
const DELETE_POLL_ATTEMPTS = 60;
type PageSize = (typeof PAGE_SIZE_OPTIONS)[number];
const DEFAULT_PAGE_SIZE: PageSize = 50;
type ReadinessFilter = "all" | "ready" | "blocked";
type LifecycleFilter = "all" | "active" | "inactive";
type SortKey = "library" | "workspace" | "documents" | "cost" | "calls" | "readiness" | "lifecycle";
type SortDirection = "asc" | "desc";
type SortState = {
  key: SortKey;
  direction: SortDirection;
};

type LibraryRow = {
  library: CatalogLibraryResponse;
  workspace: CatalogWorkspaceResponse;
  cost: LibraryCostSummary | null;
  costLoading: boolean;
  costError: boolean;
};

type DeleteTarget = "single" | "bulk";

function parseCost(value: string | null | undefined): number {
  const parsed = Number(value ?? "0");
  return Number.isFinite(parsed) ? parsed : 0;
}

function formatCurrency(value: number, currencyCode: string, locale: string) {
  return new Intl.NumberFormat(locale, {
    style: "currency",
    currency: currencyCode,
    maximumFractionDigits: value === 0 ? 0 : 3,
  }).format(value);
}

function formatInteger(value: number, locale: string) {
  return new Intl.NumberFormat(locale).format(value);
}

function libraryReadiness(row: LibraryRow): ReadinessFilter {
  return row.library.ingestionReadiness.ready ? "ready" : "blocked";
}

function lifecycleLabel(t: TFunction, lifecycleState: CatalogLibraryResponse["lifecycleState"]) {
  return lifecycleState === "active"
    ? t("admin.libraries.activeLifecycle")
    : t("admin.libraries.inactiveLifecycle");
}

function visibleSecondarySlug(displayName: string, slug: string): string | null {
  const normalize = (value: string) => value.toLocaleLowerCase().replace(/[^a-z0-9]+/g, "");
  return normalize(displayName) === normalize(slug) ? null : slug;
}

function isAbortError(error: unknown) {
  return error instanceof DOMException && error.name === "AbortError";
}

function delay(ms: number, signal: AbortSignal) {
  return new Promise<void>((resolve, reject) => {
    if (signal.aborted) {
      reject(new DOMException("Operation aborted", "AbortError"));
      return;
    }
    const timeoutId = window.setTimeout(resolve, ms);
    signal.addEventListener(
      "abort",
      () => {
        window.clearTimeout(timeoutId);
        reject(new DOMException("Operation aborted", "AbortError"));
      },
      { once: true },
    );
  });
}

async function waitForCatalogDeletion(operationId: string, signal: AbortSignal) {
  for (let attempt = 0; attempt < DELETE_POLL_ATTEMPTS; attempt += 1) {
    await delay(DELETE_POLL_INTERVAL_MS, signal);
    const operation = unwrap(await Ops.getAsyncOperation({ path: { operationId }, signal }));
    if (ASYNC_OPERATION_TERMINAL_STATES.has(operation.status)) {
      return operation;
    }
  }
  throw new Error("Catalog deletion operation did not finish in time");
}

export function LibrariesTab({ active }: { active: boolean }) {
  const { t, i18n } = useTranslation();
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const {
    refreshSession,
    selectWorkspaceLibrary,
  } = useApp();
  const mountedRef = useRef(true);
  const deleteAbortControllersRef = useRef<Set<AbortController>>(new Set());

  const [search, setSearch] = useState("");
  const [workspaceFilter, setWorkspaceFilter] = useState("all");
  const [readinessFilter, setReadinessFilter] = useState<ReadinessFilter>("all");
  const [lifecycleFilter, setLifecycleFilter] = useState<LifecycleFilter>("all");
  const [sortState, setSortState] = useState<SortState>(() => ({
    key: "library",
    direction: "asc",
  }));
  const [pageSize, setPageSize] = useState<PageSize>(DEFAULT_PAGE_SIZE);
  const [page, setPage] = useState(1);
  const [selectionMode, setSelectionMode] = useState(false);
  const [selectedIds, setSelectedIds] = useState<Set<string>>(() => new Set());
  const [selectedLibraryId, setSelectedLibraryId] = useState<string | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<DeleteTarget | null>(null);
  const [deletingIds, setDeletingIds] = useState<Set<string>>(() => new Set());

  useEffect(() => {
    return () => {
      mountedRef.current = false;
      deleteAbortControllersRef.current.forEach((controller) => controller.abort());
      deleteAbortControllersRef.current.clear();
    };
  }, []);

  const workspacesQuery = useQuery({
    ...queries.listCatalogWorkspacesOptions(),
    enabled: active,
  });
  const workspaces = workspacesQuery.data ?? [];

  const libraryQueries = useQueries({
    queries: workspaces.map((workspace) => ({
      ...queries.listCatalogLibrariesOptions({ path: { workspaceId: workspace.id } }),
      enabled: active && workspacesQuery.isSuccess,
    })),
  });

  const workspaceCostQueries = useQueries({
    queries: workspaces.map((workspace) => ({
      ...queries.getWorkspaceCostSummaryOptions({ query: { workspaceId: workspace.id } }),
      enabled: active && workspacesQuery.isSuccess,
    })),
  });

  const libraries = libraryQueries.flatMap((query, index) => {
    const workspace = workspaces[index];
    if (!workspace || !query.data) return [];
    return query.data.map((library) => ({ library, workspace }));
  });

  const libraryCostQueries = useQueries({
    queries: libraries.map(({ library }) => ({
      ...queries.getLibraryCostSummaryOptions({ query: { libraryId: library.id } }),
      enabled: active,
    })),
  });

  const rows: LibraryRow[] = libraries.map(({ library, workspace }, index) => {
    const costQuery = libraryCostQueries[index];
    return {
      library,
      workspace,
      cost: costQuery?.data ?? null,
      costLoading: costQuery?.isLoading ?? false,
      costError: costQuery?.isError ?? false,
    };
  }).filter((row) => !deletingIds.has(row.library.id));

  const workspaceCosts = new Map<string, WorkspaceCostSummary>();
  workspaceCostQueries.forEach((query, index) => {
    const workspace = workspaces[index];
    if (workspace && query.data) workspaceCosts.set(workspace.id, query.data);
  });

  const costCurrency = workspaceCostQueries.find((query) => query.data)?.data?.currencyCode
    ?? libraryCostQueries.find((query) => query.data)?.data?.currencyCode
    ?? "USD";

  const workspaceCostsReady = workspaceCostQueries.length > 0
    && workspaceCostQueries.every((query) => Boolean(query.data));
  const workspaceTotalCost = Array.from(workspaceCosts.values()).reduce(
    (sum, cost) => sum + parseCost(cost.totalCost),
    0,
  );
  const libraryTotalCost = rows.reduce((sum, row) => sum + parseCost(row.cost?.totalCost), 0);
  const totalCost = workspaceCostsReady ? workspaceTotalCost : libraryTotalCost;
  const totalDocuments = workspaceCostsReady
    ? Array.from(workspaceCosts.values()).reduce((sum, cost) => sum + cost.documentCount, 0)
    : rows.reduce((sum, row) => sum + (row.cost?.documentCount ?? 0), 0);
  const totalProviderCalls = workspaceCostsReady
    ? Array.from(workspaceCosts.values()).reduce((sum, cost) => sum + cost.providerCallCount, 0)
    : rows.reduce((sum, row) => sum + (row.cost?.providerCallCount ?? 0), 0);

  const effectiveSelectedLibraryId =
    selectedLibraryId && rows.some((row) => row.library.id === selectedLibraryId)
      ? selectedLibraryId
      : rows[0]?.library.id ?? null;
  const selectedRow = rows.find((row) => row.library.id === effectiveSelectedLibraryId) ?? null;
  const selectedRows = rows.filter((row) => selectedIds.has(row.library.id));
  const readinessCounts = useMemo(() => ({
    all: rows.length,
    ready: rows.filter((row) => libraryReadiness(row) === "ready").length,
    blocked: rows.filter((row) => libraryReadiness(row) === "blocked").length,
  }), [rows]);
  const lifecycleCounts = useMemo(() => ({
    all: rows.length,
    active: rows.filter((row) => row.library.lifecycleState === "active").length,
    inactive: rows.filter((row) => row.library.lifecycleState !== "active").length,
  }), [rows]);

  const filteredRows = useMemo(() => {
    const normalizedSearch = search.trim().toLowerCase();
    return rows
      .filter((row) => {
        if (workspaceFilter !== "all" && row.workspace.id !== workspaceFilter) return false;
        if (readinessFilter !== "all" && libraryReadiness(row) !== readinessFilter) return false;
        if (lifecycleFilter === "active" && row.library.lifecycleState !== "active") return false;
        if (lifecycleFilter === "inactive" && row.library.lifecycleState === "active") return false;
        if (!normalizedSearch) return true;
        return [
          row.library.displayName,
          row.library.slug,
          row.workspace.displayName,
          row.workspace.slug,
        ].some((value) => value.toLowerCase().includes(normalizedSearch));
      })
      .sort((left, right) => {
        const direction = sortState.direction === "asc" ? 1 : -1;
        if (sortState.key === "documents") {
          return ((left.cost?.documentCount ?? 0) - (right.cost?.documentCount ?? 0)) * direction;
        }
        if (sortState.key === "cost") {
          return (parseCost(left.cost?.totalCost) - parseCost(right.cost?.totalCost)) * direction;
        }
        if (sortState.key === "calls") {
          return ((left.cost?.providerCallCount ?? 0) - (right.cost?.providerCallCount ?? 0)) * direction;
        }
        if (sortState.key === "readiness") {
          return libraryReadiness(left).localeCompare(libraryReadiness(right), i18n.language) * direction;
        }
        if (sortState.key === "lifecycle") {
          return left.library.lifecycleState.localeCompare(right.library.lifecycleState, i18n.language) * direction;
        }
        const leftValue = sortState.key === "workspace" ? left.workspace.displayName : left.library.displayName;
        const rightValue = sortState.key === "workspace" ? right.workspace.displayName : right.library.displayName;
        return leftValue.localeCompare(rightValue, i18n.language) * direction;
      });
  }, [i18n.language, lifecycleFilter, readinessFilter, rows, search, sortState, workspaceFilter]);

  const totalPages = Math.max(1, Math.ceil(filteredRows.length / pageSize));
  const currentPage = Math.min(page, totalPages);
  const pageRows = filteredRows.slice((currentPage - 1) * pageSize, currentPage * pageSize);
  const allVisibleSelected =
    pageRows.length > 0 && pageRows.every((row) => selectedIds.has(row.library.id));

  const loading =
    workspacesQuery.isLoading ||
    libraryQueries.some((query) => query.isLoading);
  const loadError =
    workspacesQuery.error ??
    libraryQueries.find((query) => query.error)?.error ??
    null;

  const toggleSort = (nextSort: SortKey) => {
    setSortState((current) => current.key === nextSort
      ? { key: nextSort, direction: current.direction === "asc" ? "desc" : "asc" }
      : { key: nextSort, direction: "asc" });
  };

  const toggleRowSelection = (libraryId: string) => {
    setSelectedIds((current) => {
      const next = new Set(current);
      if (next.has(libraryId)) next.delete(libraryId);
      else next.add(libraryId);
      return next;
    });
  };

  const toggleVisibleSelection = () => {
    setSelectedIds((current) => {
      const next = new Set(current);
      for (const row of pageRows) {
        if (allVisibleSelected) next.delete(row.library.id);
        else next.add(row.library.id);
      }
      return next;
    });
  };

  const cancelSelection = () => {
    setSelectionMode(false);
    setSelectedIds(new Set());
  };

  const invalidateCatalog = useCallback(async () => {
    await Promise.all([
      queryClient.invalidateQueries({
        predicate: (query) => {
          const key = query.queryKey[0];
          return Boolean(key && typeof key === "object" && "_id" in key && key._id === "listCatalogWorkspaces");
        },
      }),
      queryClient.invalidateQueries({
        predicate: (query) => {
          const key = query.queryKey[0];
          return Boolean(key && typeof key === "object" && "_id" in key && key._id === "listCatalogLibraries");
        },
      }),
      queryClient.invalidateQueries({
        predicate: (query) => {
          const key = query.queryKey[0];
          return Boolean(key && typeof key === "object" && "_id" in key && (
            key._id === "getLibraryCostSummary" || key._id === "getWorkspaceCostSummary"
          ));
        },
      }),
    ]);
  }, [queryClient]);

  const openDocuments = (row: LibraryRow) => {
    const selected = selectWorkspaceLibrary(row.workspace.id, row.library.id);
    if (!selected) {
      toast.error(t("admin.libraries.openDocumentsFailed"));
      return;
    }
    void navigate("/documents");
  };

  // Rows link into the Library Hub (ADM-02) — the per-library detail route
  // that absorbs operations, ingest jobs, backup, and MCP settings.
  const openHub = (row: LibraryRow) => {
    void navigate(`/admin/library/${row.library.id}`);
  };

  const exportRows = (targetRows: LibraryRow[]) => {
    for (const row of targetRows) {
      librarySnapshotApi.downloadExport(row.library.id, ["library_data", "blobs"]);
    }
    toast.success(t("admin.libraries.exportStarted", { count: targetRows.length }));
  };

  const deleteRows = async (targetRows: LibraryRow[]) => {
    setDeleteTarget(null);
    setSelectedIds(new Set());
    setDeletingIds((current) => {
      const next = new Set(current);
      targetRows.forEach((row) => next.add(row.library.id));
      return next;
    });

    const toastId = toast.loading(t("admin.libraries.deleteStarted", { count: targetRows.length }));
    const controller = new AbortController();
    deleteAbortControllersRef.current.add(controller);
    try {
      const admissions = await Promise.all(
        targetRows.map((row) =>
          Catalog.deleteCatalogLibrary({
            path: { workspaceId: row.workspace.id, libraryId: row.library.id },
          }).then((result) => unwrap(result)),
        ),
      );

      void Promise.all(admissions.map((admission) => waitForCatalogDeletion(admission.operationId, controller.signal)))
        .then(async (operations) => {
          if (!mountedRef.current) return;
          if (operations.every((operation) => operation.status === "ready")) {
            toast.success(t("admin.libraries.deleteCompleted", { count: targetRows.length }), { id: toastId });
          } else {
            toast.error(t("admin.libraries.deleteFailed"), { id: toastId });
          }
          await invalidateCatalog();
          await refreshSession();
        })
        .catch(async (error: unknown) => {
          if (!mountedRef.current || isAbortError(error)) return;
          toast.error(errorMessage(error, t("admin.libraries.deleteFailed")), { id: toastId });
          await invalidateCatalog();
          await refreshSession();
        })
        .finally(() => {
          deleteAbortControllersRef.current.delete(controller);
        });
    } catch (error: unknown) {
      deleteAbortControllersRef.current.delete(controller);
      if (!mountedRef.current) return;
      toast.error(errorMessage(error, t("admin.libraries.deleteFailed")), { id: toastId });
      setDeletingIds((current) => {
        const next = new Set(current);
        targetRows.forEach((row) => next.delete(row.library.id));
        return next;
      });
    }
  };

  return (
    <div className="flex h-full min-h-0 flex-col overflow-auto xl:overflow-hidden">
      <LibrariesSummary
        currencyCode={costCurrency}
        locale={i18n.language}
        totalCost={totalCost}
        totalDocuments={totalDocuments}
        totalLibraries={rows.length}
        totalProviderCalls={totalProviderCalls}
        totalWorkspaces={workspaces.length}
        t={t}
      />
      <LibrariesFilters
        lifecycleFilter={lifecycleFilter}
        lifecycleCounts={lifecycleCounts}
        onLifecycleFilterChange={(value) => {
          setLifecycleFilter(value);
          setPage(1);
        }}
        onReadinessFilterChange={(value) => {
          setReadinessFilter(value);
          setPage(1);
        }}
        onSearchChange={(value) => {
          setSearch(value);
          setPage(1);
        }}
        onSelectionCancel={cancelSelection}
        onSelectionStart={() => setSelectionMode(true)}
        onWorkspaceFilterChange={(value) => {
          setWorkspaceFilter(value);
          setPage(1);
        }}
        readinessFilter={readinessFilter}
        readinessCounts={readinessCounts}
        search={search}
        selectionMode={selectionMode}
        t={t}
        workspaceFilter={workspaceFilter}
        workspaces={workspaces}
      />
      <DataState
        query={{
          isLoading: loading && rows.length === 0,
          error: loadError ? errorMessage(loadError, t("admin.libraries.loadFailed")) : null,
          data: rows,
        }}
        loading={<LibrariesLoading t={t} />}
        errorRender={(error) => (
          <LibrariesError
            error={String(error)}
            onRetry={() => void workspacesQuery.refetch()}
            t={t}
          />
        )}
        emptyCheck={() => rows.length === 0}
        emptyRender={<LibrariesEmpty t={t} />}
      >
        {() => (
          <div className="grid min-h-[34rem] flex-none grid-cols-1 overflow-visible xl:min-h-0 xl:flex-1 xl:grid-cols-[minmax(0,1fr)_22rem] xl:overflow-hidden">
            <div className="flex min-w-0 min-h-0 flex-col overflow-hidden xl:border-r">
              <div className="min-h-[22rem] flex-1 overflow-auto xl:min-h-0">
                <LibrariesTable
                  allVisibleSelected={allVisibleSelected}
                  currencyCode={costCurrency}
                  locale={i18n.language}
                  onDelete={(row) => {
                    setSelectedLibraryId(row.library.id);
                    setDeleteTarget("single");
                  }}
                  onOpenDocuments={openDocuments}
                  onOpenHub={openHub}
                  onSelectRow={(row) => {
                    if (selectionMode) {
                      toggleRowSelection(row.library.id);
                      return;
                    }
                    setSelectedLibraryId(row.library.id);
                  }}
                  onToggleSelection={toggleRowSelection}
                  onToggleSort={toggleSort}
                  onToggleVisibleSelection={toggleVisibleSelection}
                  pageRows={pageRows}
                  selectedIds={selectedIds}
                  selectedLibraryId={effectiveSelectedLibraryId}
                  selectionMode={selectionMode}
                  sortDirection={sortState.direction}
                  sortKey={sortState.key}
                  t={t}
                />
              </div>
              <LibrariesBulkBar
                onClear={cancelSelection}
                onDelete={() => setDeleteTarget("bulk")}
                onExport={() => exportRows(selectedRows)}
                selectedCount={selectedIds.size}
                t={t}
              />
              <LibrariesPagination
                currentPage={currentPage}
                filteredCount={filteredRows.length}
                onPageChange={setPage}
                onPageSizeChange={(value) => {
                  setPageSize(value);
                  setPage(1);
                }}
                pageSize={pageSize}
                t={t}
                totalPages={totalPages}
                visibleEnd={Math.min(currentPage * pageSize, filteredRows.length)}
                visibleStart={filteredRows.length === 0 ? 0 : ((currentPage - 1) * pageSize) + 1}
              />
            </div>
            <LibraryInspector
              currencyCode={costCurrency}
              locale={i18n.language}
              onDelete={(row) => {
                setSelectedLibraryId(row.library.id);
                setDeleteTarget("single");
              }}
              onOpenDocuments={openDocuments}
              onOpenHub={openHub}
              row={selectedRow}
              t={t}
            />
          </div>
        )}
      </DataState>
      <ConfirmDeleteDialog
        count={deleteTarget === "bulk" ? selectedRows.length : selectedRow ? 1 : 0}
        onCancel={() => setDeleteTarget(null)}
        onConfirm={() => {
          const targetRows = deleteTarget === "bulk" ? selectedRows : selectedRow ? [selectedRow] : [];
          void deleteRows(targetRows);
        }}
        open={deleteTarget !== null}
        t={t}
      />
    </div>
  );
}

function LibrariesSummary({
  currencyCode,
  locale,
  totalCost,
  totalDocuments,
  totalLibraries,
  totalProviderCalls,
  totalWorkspaces,
  t,
}: {
  currencyCode: string;
  locale: string;
  totalCost: number;
  totalDocuments: number;
  totalLibraries: number;
  totalProviderCalls: number;
  totalWorkspaces: number;
  t: TFunction;
}) {
  const cards = [
    {
      label: t("admin.libraries.totalCost"),
      value: formatCurrency(totalCost, currencyCode, locale),
      icon: Database,
      iconClass: "bg-primary/10 text-primary",
    },
    {
      label: t("admin.libraries.workspaces"),
      value: formatInteger(totalWorkspaces, locale),
      icon: BookOpen,
      iconClass: "bg-status-sparse-bg text-status-sparse",
    },
    {
      label: t("admin.libraries.libraries"),
      value: formatInteger(totalLibraries, locale),
      icon: FileText,
      iconClass: "bg-status-ready-bg text-status-ready",
    },
    {
      label: t("admin.libraries.documents"),
      value: formatInteger(totalDocuments, locale),
      icon: CheckSquare,
      iconClass: "bg-status-processing-bg text-status-processing",
    },
    {
      label: t("admin.libraries.providerCalls"),
      value: formatInteger(totalProviderCalls, locale),
      icon: RotateCw,
      iconClass: "bg-status-warning-bg text-status-warning",
    },
  ];

  return (
    <div className="border-b bg-surface-sunken/50 px-6 py-4">
      <div className="grid grid-cols-2 gap-3 md:grid-cols-5">
        {cards.map((card) => (
          <div key={card.label} className="rounded-lg border bg-card px-4 py-3 shadow-soft">
            <div className="flex items-center gap-2">
              <span className={`flex h-7 w-7 items-center justify-center rounded-md ${card.iconClass}`}>
                <card.icon className="h-3.5 w-3.5" />
              </span>
              <span className="text-[11px] font-semibold uppercase text-muted-foreground">
                {card.label}
              </span>
            </div>
            <div className="mt-1 text-lg font-bold tabular-nums tracking-tight">
              {card.value}
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}

function LibrariesFilters({
  lifecycleFilter,
  lifecycleCounts,
  onLifecycleFilterChange,
  onReadinessFilterChange,
  onSearchChange,
  onSelectionCancel,
  onSelectionStart,
  onWorkspaceFilterChange,
  readinessFilter,
  readinessCounts,
  search,
  selectionMode,
  t,
  workspaceFilter,
  workspaces,
}: {
  lifecycleFilter: LifecycleFilter;
  lifecycleCounts: Record<LifecycleFilter, number>;
  onLifecycleFilterChange: (value: LifecycleFilter) => void;
  onReadinessFilterChange: (value: ReadinessFilter) => void;
  onSearchChange: (value: string) => void;
  onSelectionCancel: () => void;
  onSelectionStart: () => void;
  onWorkspaceFilterChange: (value: string) => void;
  readinessFilter: ReadinessFilter;
  readinessCounts: Record<ReadinessFilter, number>;
  search: string;
  selectionMode: boolean;
  t: TFunction;
  workspaceFilter: string;
  workspaces: CatalogWorkspaceResponse[];
}) {
  const readinessOptions = [
    { key: "all" as const, label: t("admin.libraries.allReadiness"), count: readinessCounts.all, icon: <Database className="h-3 w-3 text-primary" /> },
    { key: "ready" as const, label: t("admin.libraries.ready"), count: readinessCounts.ready, icon: <CheckCircle2 className="h-3 w-3 text-status-ready" /> },
    { key: "blocked" as const, label: t("admin.libraries.blocked"), count: readinessCounts.blocked, icon: <XCircle className="h-3 w-3 text-status-failed" /> },
  ];
  const lifecycleOptions = [
    { key: "all" as const, label: t("admin.libraries.allLifecycle"), count: lifecycleCounts.all, icon: <Filter className="h-3 w-3 text-primary" /> },
    { key: "active" as const, label: t("admin.libraries.activeLifecycle"), count: lifecycleCounts.active, icon: <CheckCircle2 className="h-3 w-3 text-status-ready" /> },
    { key: "inactive" as const, label: t("admin.libraries.inactiveLifecycle"), count: lifecycleCounts.inactive, icon: <Ban className="h-3 w-3 text-status-stalled" /> },
  ];

  return (
    <div className="flex flex-wrap items-center gap-3 border-b bg-surface-sunken/50 px-6 py-3">
      <div className="relative min-w-[220px] flex-1 max-w-lg">
        <Search className="pointer-events-none absolute left-3 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
        <Input
          className="h-9 rounded-lg bg-card pl-9 text-sm shadow-soft"
          onChange={(event) => onSearchChange(event.target.value)}
          placeholder={t("admin.libraries.searchPlaceholder")}
          value={search}
        />
      </div>
      <FilterSelect value={workspaceFilter} onValueChange={onWorkspaceFilterChange} icon={<Filter className="h-3.5 w-3.5" />}>
        <SelectItem value="all">{t("admin.libraries.allWorkspaces")}</SelectItem>
        {workspaces.map((workspace) => (
          <SelectItem key={workspace.id} value={workspace.id}>
            {workspace.displayName}
          </SelectItem>
        ))}
      </FilterSelect>
      <div className="flex flex-wrap gap-0.5 rounded-lg border border-border/50 bg-muted p-1">
        {readinessOptions.map((option) => (
          <FilterChip
            active={readinessFilter === option.key}
            count={option.count}
            icon={option.icon}
            key={option.key}
            label={option.label}
            onClick={() => onReadinessFilterChange(option.key)}
          />
        ))}
      </div>
      <div className="flex flex-wrap gap-0.5 rounded-lg border border-border/50 bg-muted p-1">
        {lifecycleOptions.map((option) => (
          <FilterChip
            active={lifecycleFilter === option.key}
            count={option.count}
            icon={option.icon}
            key={option.key}
            label={option.label}
            onClick={() => onLifecycleFilterChange(option.key)}
          />
        ))}
      </div>
      <Button
        size="sm"
        variant={selectionMode ? "default" : "outline"}
        className="ml-auto h-8 text-xs"
        onClick={selectionMode ? onSelectionCancel : onSelectionStart}
      >
        <CheckSquare className="mr-1.5 h-3.5 w-3.5" />
        {selectionMode ? t("admin.libraries.cancelSelection") : t("admin.libraries.select")}
      </Button>
    </div>
  );
}

function FilterSelect({
  children,
  icon,
  onValueChange,
  value,
}: {
  children: ReactNode;
  icon?: ReactNode;
  onValueChange: (value: string) => void;
  value: string;
}) {
  return (
    <Select value={value} onValueChange={onValueChange}>
      <SelectTrigger className="h-9 w-[220px] rounded-lg bg-card text-xs shadow-soft">
        <span className="flex min-w-0 items-center gap-1.5">
          {icon}
          <SelectValue />
        </span>
      </SelectTrigger>
      <SelectContent>{children}</SelectContent>
    </Select>
  );
}

function FilterChip({
  active,
  count,
  icon,
  label,
  onClick,
}: {
  active: boolean;
  count: number;
  icon: ReactNode;
  label: string;
  onClick: () => void;
}) {
  return (
    <button
      className={`flex items-center gap-1.5 rounded-md px-3 py-1.5 text-xs font-medium transition-all duration-200 ${
        active
          ? "bg-card text-foreground shadow-soft"
          : "text-muted-foreground hover:text-foreground"
      }`}
      onClick={onClick}
      type="button"
    >
      {icon}
      {label}
      {count > 0 && (
        <span className="tabular-nums text-[10px] opacity-70">
          {count}
        </span>
      )}
    </button>
  );
}

function LibrariesTable({
  allVisibleSelected,
  currencyCode,
  locale,
  onDelete,
  onExport,
  onOpenDocuments,
  onOpenHub,
  onSelectRow,
  onToggleSelection,
  onToggleSort,
  onToggleVisibleSelection,
  pageRows,
  selectedIds,
  selectedLibraryId,
  selectionMode,
  sortDirection,
  sortKey,
  t,
}: {
  allVisibleSelected: boolean;
  currencyCode: string;
  locale: string;
  onDelete: (row: LibraryRow) => void;
  onOpenDocuments: (row: LibraryRow) => void;
  onOpenHub: (row: LibraryRow) => void;
  onSelectRow: (row: LibraryRow) => void;
  onToggleSelection: (libraryId: string) => void;
  onToggleSort: (key: SortKey) => void;
  onToggleVisibleSelection: () => void;
  pageRows: LibraryRow[];
  selectedIds: Set<string>;
  selectedLibraryId: string | null;
  selectionMode: boolean;
  sortDirection: SortDirection;
  sortKey: SortKey;
  t: TFunction;
}) {
  const sortIcon = sortDirection === "asc" ? <ArrowUp className="h-3 w-3" /> : <ArrowDown className="h-3 w-3" />;

  if (pageRows.length === 0) {
    return <div className="empty-state py-20">{t("admin.libraries.noMatches")}</div>;
  }

  return (
    <table className="w-full min-w-[1180px] table-fixed text-sm">
      <colgroup>
        {selectionMode && <col className="w-12" />}
        <col className="w-72" />
        <col className="w-52" />
        <col className="w-24" />
        <col className="w-28" />
        <col className="w-24" />
        <col className="w-36" />
        <col className="w-32" />
        <col className="w-32" />
      </colgroup>
      <thead
        className="sticky top-0 z-10"
        style={{
          background: "linear-gradient(180deg, hsl(var(--card)), hsl(var(--card) / 0.95))",
          backdropFilter: "blur(8px)",
        }}
      >
        <tr className="border-b text-left">
          {selectionMode && (
            <th className="px-4 py-3 w-10">
              <input
                type="checkbox"
                checked={allVisibleSelected}
                onChange={onToggleVisibleSelection}
                className="h-4 w-4 rounded border-gray-300"
                aria-label={t("admin.libraries.selectVisible")}
              />
            </th>
          )}
          <SortHeader active={sortKey === "library"} description={t("admin.libraries.columnHelp.library")} icon={sortIcon} label={t("admin.libraries.library")} onClick={() => onToggleSort("library")} />
          <SortHeader active={sortKey === "workspace"} description={t("admin.libraries.columnHelp.workspace")} icon={sortIcon} label={t("admin.libraries.workspace")} onClick={() => onToggleSort("workspace")} />
          <SortHeader active={sortKey === "documents"} description={t("admin.libraries.columnHelp.documents")} icon={sortIcon} label={t("admin.libraries.documents")} onClick={() => onToggleSort("documents")} />
          <SortHeader active={sortKey === "cost"} description={t("admin.libraries.columnHelp.cost")} icon={sortIcon} label={t("admin.libraries.cost")} onClick={() => onToggleSort("cost")} />
          <SortHeader active={sortKey === "calls"} description={t("admin.libraries.columnHelp.calls")} icon={sortIcon} label={t("admin.libraries.calls")} onClick={() => onToggleSort("calls")} />
          <SortHeader active={sortKey === "readiness"} description={t("admin.libraries.columnHelp.readiness")} icon={sortIcon} label={t("admin.libraries.readiness")} onClick={() => onToggleSort("readiness")} />
          <SortHeader active={sortKey === "lifecycle"} description={t("admin.libraries.columnHelp.lifecycle")} icon={sortIcon} label={t("admin.libraries.lifecycle")} onClick={() => onToggleSort("lifecycle")} />
          <ColumnHeader description={t("admin.libraries.columnHelp.actions")} label={t("admin.libraries.actions")} />
        </tr>
      </thead>
      <tbody>
        {pageRows.map((row) => (
          <tr
            key={row.library.id}
            aria-selected={selectedLibraryId === row.library.id}
            className={`border-b cursor-pointer transition-all duration-150 ${
              selectedIds.has(row.library.id)
                ? "bg-primary/10"
                : selectedLibraryId === row.library.id
                  ? "bg-primary/5 border-l-2 border-l-primary"
                  : "hover:bg-accent/30"
            }`}
            onKeyDown={rowKeyHandler(() => onSelectRow(row))}
            onClick={() => onSelectRow(row)}
            tabIndex={0}
          >
            {selectionMode && (
              <td className="px-4 py-3.5 w-10">
                <input
                  type="checkbox"
                  checked={selectedIds.has(row.library.id)}
                  onChange={(event) => {
                    event.stopPropagation();
                    onToggleSelection(row.library.id);
                  }}
                  onClick={(event) => event.stopPropagation()}
                  className="h-4 w-4 rounded border-gray-300"
                  aria-label={t("admin.libraries.selectLibrary", { name: row.library.displayName })}
                />
              </td>
            )}
            <td className="px-4 py-3.5">
              <LibraryNameCell library={row.library} />
            </td>
            <td className="px-4 py-3.5">
              <NameWithOptionalSlug displayName={row.workspace.displayName} slug={row.workspace.slug} />
            </td>
            <td className="px-4 py-3.5 text-xs tabular-nums text-muted-foreground">
              {row.costLoading ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : formatInteger(row.cost?.documentCount ?? 0, locale)}
            </td>
            <td className="px-4 py-3.5 text-xs tabular-nums text-muted-foreground">
              {row.costError ? t("admin.libraries.costUnavailable") : formatCurrency(parseCost(row.cost?.totalCost), row.cost?.currencyCode ?? currencyCode, locale)}
            </td>
            <td className="px-4 py-3.5 text-xs tabular-nums text-muted-foreground">
              {formatInteger(row.cost?.providerCallCount ?? 0, locale)}
            </td>
            <td className="px-4 py-3.5">
              <ReadinessBadge row={row} t={t} />
            </td>
            <td className="px-4 py-3.5 text-xs text-muted-foreground">
              <LifecycleBadge lifecycleState={row.library.lifecycleState} t={t} />
            </td>
            <td className="px-4 py-3.5">
              <div className="flex items-center gap-1">
                <IconButton label={t("admin.libraries.openHub")} onClick={(event) => { event.stopPropagation(); onOpenHub(row); }}>
                  <Settings2 className="h-3.5 w-3.5" />
                </IconButton>
                <IconButton label={t("admin.libraries.openDocuments")} onClick={(event) => { event.stopPropagation(); onOpenDocuments(row); }}>
                  <ExternalLink className="h-3.5 w-3.5" />
                </IconButton>
                <IconButton destructive label={t("admin.libraries.delete")} onClick={(event) => { event.stopPropagation(); onDelete(row); }}>
                  <Trash2 className="h-3.5 w-3.5" />
                </IconButton>
              </div>
            </td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

function NameWithOptionalSlug({
  displayName,
  slug,
}: {
  displayName: string;
  slug: string;
}) {
  const secondarySlug = visibleSecondarySlug(displayName, slug);
  return (
    <div className="min-w-0">
      <span className="block truncate text-sm font-semibold" title={displayName}>
        {displayName}
      </span>
      {secondarySlug && (
        <span className="block truncate font-mono text-[10px] text-muted-foreground" title={secondarySlug}>
          {secondarySlug}
        </span>
      )}
    </div>
  );
}

function LibraryNameCell({ library }: { library: CatalogLibraryResponse }) {
  return (
    <div className="flex min-w-0 items-center gap-3">
      <div className="flex h-8 w-8 shrink-0 items-center justify-center rounded-lg bg-surface-sunken">
        <Database className="h-3.5 w-3.5 text-muted-foreground" />
      </div>
      <NameWithOptionalSlug displayName={library.displayName} slug={library.slug} />
    </div>
  );
}

function SortHeader({
  active,
  description,
  icon,
  label,
  onClick,
}: {
  active: boolean;
  description: string;
  icon: JSX.Element;
  label: string;
  onClick: () => void;
}) {
  return (
    <th className="px-4 py-3 section-label">
      <Tooltip>
        <TooltipTrigger asChild>
          <button
            aria-label={`${label}: ${description}`}
            className="inline-flex items-center gap-1 rounded-sm hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2"
            onClick={onClick}
            type="button"
          >
            {label}
            {active && icon}
            <HelpCircle className="h-3 w-3 text-muted-foreground/70" aria-hidden="true" />
          </button>
        </TooltipTrigger>
        <TooltipContent align="start" className="max-w-72 normal-case tracking-normal">
          {description}
        </TooltipContent>
      </Tooltip>
    </th>
  );
}

function ColumnHeader({
  description,
  label,
}: {
  description: string;
  label: string;
}) {
  return (
    <th className="px-4 py-3 section-label">
      <Tooltip>
        <TooltipTrigger asChild>
          <span
            aria-label={`${label}: ${description}`}
            className="inline-flex cursor-help items-center gap-1 rounded-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2"
            tabIndex={0}
          >
            {label}
            <HelpCircle className="h-3 w-3 text-muted-foreground/70" aria-hidden="true" />
          </span>
        </TooltipTrigger>
        <TooltipContent align="start" className="max-w-72 normal-case tracking-normal">
          {description}
        </TooltipContent>
      </Tooltip>
    </th>
  );
}

function rowKeyHandler(action: () => void): KeyboardEventHandler<HTMLTableRowElement> {
  return (event) => {
    if (event.key !== "Enter" && event.key !== " ") return;
    event.preventDefault();
    action();
  };
}

function ReadinessBadge({ row, t }: { row: LibraryRow; t: TFunction }) {
  const ready = row.library.ingestionReadiness.ready;
  const missingCount = row.library.ingestionReadiness.missingBindingPurposes.length;
  return (
    <span
      className={`status-badge whitespace-nowrap ${
        ready ? "status-ready" : "status-failed"
      }`}
      title={missingCount > 0 ? row.library.ingestionReadiness.missingBindingPurposes.join(", ") : undefined}
    >
      {ready ? <CheckCircle2 className="h-3 w-3" /> : <XCircle className="h-3 w-3" />}
      {ready ? t("admin.libraries.ready") : t("admin.libraries.blocked")}
      {!ready && missingCount > 0 ? ` · ${missingCount}` : ""}
    </span>
  );
}

function LifecycleBadge({
  lifecycleState,
  t,
}: {
  lifecycleState: CatalogLibraryResponse["lifecycleState"];
  t: TFunction;
}) {
  const active = lifecycleState === "active";
  return (
    <span className={`status-badge whitespace-nowrap ${active ? "status-ready" : "status-stalled"}`}>
      {active ? <CheckCircle2 className="h-3 w-3" /> : <Ban className="h-3 w-3" />}
      {lifecycleLabel(t, lifecycleState)}
    </span>
  );
}

function IconButton({
  children,
  destructive = false,
  label,
  onClick,
}: {
  children: ReactNode;
  destructive?: boolean;
  label: string;
  onClick: MouseEventHandler<HTMLButtonElement>;
}) {
  return (
    <button
      aria-label={label}
      className={`inline-flex h-8 w-8 items-center justify-center rounded-md border transition-colors ${
        destructive
          ? "text-destructive hover:bg-destructive/10"
          : "text-muted-foreground hover:bg-accent hover:text-foreground"
      }`}
      onClick={onClick}
      title={label}
      type="button"
    >
      {children}
    </button>
  );
}

function LibrariesPagination({
  currentPage,
  filteredCount,
  onPageChange,
  onPageSizeChange,
  pageSize,
  t,
  totalPages,
  visibleEnd,
  visibleStart,
}: {
  currentPage: number;
  filteredCount: number;
  onPageChange: (page: number) => void;
  onPageSizeChange: (pageSize: PageSize) => void;
  pageSize: PageSize;
  t: TFunction;
  totalPages: number;
  visibleEnd: number;
  visibleStart: number;
}) {
  return (
    <TablePaginationFooter
      canGoNext={currentPage < totalPages}
      canGoPrevious={currentPage > 1}
      currentPageNumber={currentPage}
      goToNextPage={() => onPageChange(currentPage + 1)}
      goToPage={onPageChange}
      goToPreviousPage={() => onPageChange(currentPage - 1)}
      nextLabel={t("admin.libraries.next")}
      onPageSizeChange={onPageSizeChange}
      pageSize={pageSize}
      pageSizeLabel={t("admin.libraries.pageSize")}
      pageSizeOptions={PAGE_SIZE_OPTIONS}
      previousLabel={t("admin.libraries.previous")}
      summary={t("admin.libraries.paginationSummary", {
        count: filteredCount,
        from: visibleStart,
        to: visibleEnd,
        total: filteredCount,
      })}
      totalPages={totalPages}
    />
  );
}

function LibraryInspector({
  currencyCode,
  locale,
  onDelete,
  onOpenDocuments,
  onOpenHub,
  row,
  t,
}: {
  currencyCode: string;
  locale: string;
  onDelete: (row: LibraryRow) => void;
  onOpenDocuments: (row: LibraryRow) => void;
  onOpenHub: (row: LibraryRow) => void;
  row: LibraryRow | null;
  t: TFunction;
}) {
  if (!row) {
    return (
      <aside className="hidden min-h-0 border-l bg-surface-sunken/40 p-5 xl:block">
        <div className="empty-state py-12 text-sm">{t("admin.libraries.inspectorEmpty")}</div>
      </aside>
    );
  }

  return (
    <aside className="min-h-0 overflow-auto bg-surface-sunken/40 p-5">
      <div className="rounded-lg border bg-card p-4 shadow-soft">
        <div className="section-label">{t("admin.libraries.inspectorTitle")}</div>
        <h2 className="mt-2 truncate text-base font-bold tracking-tight" title={row.library.displayName}>
          {row.library.displayName}
        </h2>
        <p className="mt-1 truncate text-xs text-muted-foreground" title={row.workspace.displayName}>
          {row.workspace.displayName}
        </p>
        <div className="mt-4 space-y-3 text-sm">
          <InspectorMetric label={t("admin.libraries.documents")} value={formatInteger(row.cost?.documentCount ?? 0, locale)} />
          <InspectorMetric label={t("admin.libraries.cost")} value={formatCurrency(parseCost(row.cost?.totalCost), row.cost?.currencyCode ?? currencyCode, locale)} />
          <InspectorMetric label={t("admin.libraries.calls")} value={formatInteger(row.cost?.providerCallCount ?? 0, locale)} />
          <InspectorMetric label={t("admin.libraries.lifecycle")} value={lifecycleLabel(t, row.library.lifecycleState)} />
          <InspectorMetric label={t("admin.libraries.libraryId")} value={row.library.id} mono />
          <InspectorMetric label={t("admin.libraries.workspaceId")} value={row.workspace.id} mono />
        </div>
        <div className="mt-4 flex flex-col gap-2">
          <Button onClick={() => onOpenHub(row)} size="sm">
            <Settings2 className="mr-1.5 h-3.5 w-3.5" />
            {t("admin.libraries.openHub")}
          </Button>
          <Button onClick={() => onOpenDocuments(row)} size="sm" variant="outline">
            <ExternalLink className="mr-1.5 h-3.5 w-3.5" />
            {t("admin.libraries.openDocuments")}
          </Button>
          <Button onClick={() => onDelete(row)} size="sm" variant="destructive">
            <Trash2 className="mr-1.5 h-3.5 w-3.5" />
            {t("admin.libraries.delete")}
          </Button>
        </div>
      </div>
    </aside>
  );
}

function InspectorMetric({
  label,
  mono = false,
  value,
}: {
  label: string;
  mono?: boolean;
  value: string;
}) {
  return (
    <div className="min-w-0">
      <div className="text-[10px] font-semibold uppercase text-muted-foreground">{label}</div>
      <div className={`mt-0.5 truncate ${mono ? "font-mono text-xs" : "font-semibold"}`} title={value}>
        {value}
      </div>
    </div>
  );
}

function LibrariesBulkBar({
  onClear,
  onDelete,
  onExport,
  selectedCount,
  t,
}: {
  onClear: () => void;
  onDelete: () => void;
  onExport: () => void;
  selectedCount: number;
  t: TFunction;
}) {
  if (selectedCount <= 0) return null;
  return (
    <div className="flex flex-wrap items-center gap-3 border-t bg-background px-4 py-3 shadow-lg">
      <span className="text-sm font-medium tabular-nums">
        {t("admin.libraries.selected", { count: selectedCount })}
      </span>
      <Button onClick={onExport} size="sm" variant="outline">
        <Download className="mr-1.5 h-3.5 w-3.5" />
        {t("admin.libraries.exportSelected")}
      </Button>
      <Button onClick={onDelete} size="sm" variant="destructive">
        <Trash2 className="mr-1.5 h-3.5 w-3.5" />
        {t("admin.libraries.deleteSelected")}
      </Button>
      <div className="flex-1" />
      <Button onClick={onClear} size="sm" variant="ghost">
        {t("admin.libraries.clearSelection")}
      </Button>
    </div>
  );
}

function ConfirmDeleteDialog({
  count,
  onCancel,
  onConfirm,
  open,
  t,
}: {
  count: number;
  onCancel: () => void;
  onConfirm: () => void;
  open: boolean;
  t: TFunction;
}) {
  return (
    <Dialog open={open} onOpenChange={(nextOpen) => { if (!nextOpen) onCancel(); }}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t("admin.libraries.deleteTitle", { count })}</DialogTitle>
          <DialogDescription>{t("admin.libraries.deleteDesc", { count })}</DialogDescription>
        </DialogHeader>
        <DialogFooter>
          <Button onClick={onCancel} variant="outline">{t("common.cancel")}</Button>
          <Button disabled={count <= 0} onClick={onConfirm} variant="destructive">
            <Trash2 className="mr-1.5 h-3.5 w-3.5" />
            {t("admin.libraries.delete")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function LibrariesLoading({ t }: { t: TFunction }) {
  return (
    <div className="empty-state py-20">
      <Loader2 className="mb-4 h-7 w-7 animate-spin text-primary" />
      <h2 className="text-base font-bold tracking-tight">{t("admin.libraries.loading")}</h2>
    </div>
  );
}

function LibrariesError({
  error,
  onRetry,
  t,
}: {
  error: string;
  onRetry: () => void;
  t: TFunction;
}) {
  return (
    <div className="empty-state py-20">
      <div className="mb-4 flex h-14 w-14 items-center justify-center rounded-lg bg-destructive/10">
        <XCircle className="h-7 w-7 text-destructive" />
      </div>
      <h2 className="text-base font-bold tracking-tight">{t("admin.libraries.loadFailed")}</h2>
      <p className="mt-2 text-sm text-muted-foreground">{error}</p>
      <Button className="mt-4" onClick={onRetry} size="sm" variant="outline">
        <RotateCw className="mr-1.5 h-3.5 w-3.5" />
        {t("documents.retry")}
      </Button>
    </div>
  );
}

function LibrariesEmpty({ t }: { t: TFunction }) {
  return (
    <div className="empty-state py-20">
      <div className="mb-4 flex h-14 w-14 items-center justify-center rounded-lg bg-muted">
        <Database className="h-7 w-7 text-muted-foreground" />
      </div>
      <h2 className="text-base font-bold tracking-tight">{t("admin.libraries.empty")}</h2>
    </div>
  );
}
