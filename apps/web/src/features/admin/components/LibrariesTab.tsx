import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { JSX, KeyboardEventHandler } from "react";
import type { TFunction } from "i18next";
import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router-dom";
import { useMutation, useQueries, useQuery, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import {
  ArrowDown,
  ArrowUp,
  Ban,
  BookOpen,
  Brain,
  Building2,
  CheckCircle2,
  CheckSquare,
  Database,
  Download,
  ExternalLink,
  FileText,
  HelpCircle,
  Loader2,
  Power,
  RotateCw,
  Search,
  Trash2,
  Upload,
  XCircle,
} from "lucide-react";

import {
  ASYNC_OPERATION_TERMINAL_STATES,
  Catalog,
  Ops,
  adminApi,
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
import { FilterSelect } from "@/shared/components/FilterSelect";
import { TablePaginationFooter } from "@/shared/components/TablePaginationFooter";
import { Button } from "@/shared/components/ui/button";
import { Checkbox } from "@/shared/components/ui/checkbox";
import { Input } from "@/shared/components/ui/input";
import { SelectItem } from "@/shared/components/ui/select";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/shared/components/ui/tooltip";
import { DataState } from "@/shared/components/DataState";
import { useApp } from "@/shared/contexts/app-context";
import { errorMessage } from "@/shared/lib/errorMessage";
import { ConfirmDialog } from "@/shared/components/layout/ConfirmDialog";
import { DataView } from "@/shared/components/layout/DataView";
import { InspectorPanel } from "@/shared/components/layout/InspectorPanel";
import { RowActionsMenu, type RowAction } from "@/shared/components/layout/RowActionsMenu";
import { WorkbenchEmptyState } from "@/shared/components/layout/WorkbenchEmptyState";
import { StatusBadge } from "@/shared/components/StatusBadge";
import { BackupExportDialog, BackupImportDialog } from "./BackupDialogs";

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
  const [inspectorOpen, setInspectorOpen] = useState(false);
  const [deleteTarget, setDeleteTarget] = useState<DeleteTarget | null>(null);
  const [deletingIds, setDeletingIds] = useState<Set<string>>(() => new Set());
  const [backupTarget, setBackupTarget] = useState<LibraryRow | null>(null);
  const [restoreTarget, setRestoreTarget] = useState<LibraryRow | null>(null);

  useEffect(() => {
    const deleteAbortControllers = deleteAbortControllersRef.current;
    return () => {
      mountedRef.current = false;
      deleteAbortControllers.forEach((controller) => controller.abort());
      deleteAbortControllers.clear();
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
  const effectiveSelectedLibraryId =
    selectedLibraryId && pageRows.some((row) => row.library.id === selectedLibraryId)
      ? selectedLibraryId
      : pageRows[0]?.library.id ?? null;
  const selectedRow = pageRows.find((row) => row.library.id === effectiveSelectedLibraryId) ?? null;
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

  // Per-library actions are surfaced directly on the catalog row + inspector so
  // they are reachable in one or two clicks. The old per-library "hub" route was
  // dissolved: backup/restore/AI live here, audit moved to the global Audit page.
  const configureAi = (row: LibraryRow) => {
    void navigate(`/admin/ai?scope=library&lib=${row.library.id}&section=bindings`);
  };

  const openBackup = (row: LibraryRow) => setBackupTarget(row);
  const openRestore = (row: LibraryRow) => setRestoreTarget(row);

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
          <DataView
            inspectorCloseLabel={t("common.close")}
            inspectorLabel={t("admin.libraries.inspectorTitle")}
            inspectorOpen={inspectorOpen}
            onInspectorOpenChange={setInspectorOpen}
            inspector={
              <LibraryInspector
                currencyCode={costCurrency}
                locale={i18n.language}
                onDelete={(row) => {
                  setSelectedLibraryId(row.library.id);
                  setDeleteTarget("single");
                }}
                onBackup={openBackup}
                onConfigureAi={configureAi}
                onOpenDocuments={openDocuments}
                onRestore={openRestore}
                row={selectedRow}
                t={t}
              />
            }
          >
              <div className="min-h-[22rem] flex-1 overflow-auto xl:min-h-0">
                <LibrariesTable
                  allVisibleSelected={allVisibleSelected}
                  currencyCode={costCurrency}
                  locale={i18n.language}
                  onDelete={(row) => {
                    setSelectedLibraryId(row.library.id);
                    setDeleteTarget("single");
                  }}
                  onBackup={openBackup}
                  onConfigureAi={configureAi}
                  onOpenDocuments={openDocuments}
                  onRestore={openRestore}
                  onSelectRow={(row) => {
                    if (selectionMode) {
                      toggleRowSelection(row.library.id);
                      return;
                    }
                    setSelectedLibraryId(row.library.id);
                    setInspectorOpen(true);
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
          </DataView>
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
      <BackupExportDialog
        open={backupTarget !== null}
        onOpenChange={(open) => {
          if (!open) setBackupTarget(null);
        }}
        libraryId={backupTarget?.library.id ?? ""}
        t={t}
      />
      <BackupImportDialog
        open={restoreTarget !== null}
        onOpenChange={(open) => {
          if (!open) setRestoreTarget(null);
        }}
        libraryId={restoreTarget?.library.id ?? ""}
        t={t}
        onCompleted={() => void invalidateCatalog()}
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
      iconClass: "bg-muted text-muted-foreground",
    },
    {
      label: t("admin.libraries.workspaces"),
      value: formatInteger(totalWorkspaces, locale),
      icon: BookOpen,
      iconClass: "bg-muted text-muted-foreground",
    },
    {
      label: t("admin.libraries.libraries"),
      value: formatInteger(totalLibraries, locale),
      icon: FileText,
      iconClass: "bg-muted text-muted-foreground",
    },
    {
      label: t("admin.libraries.documents"),
      value: formatInteger(totalDocuments, locale),
      icon: CheckSquare,
      iconClass: "bg-muted text-muted-foreground",
    },
    {
      label: t("admin.libraries.providerCalls"),
      value: formatInteger(totalProviderCalls, locale),
      icon: RotateCw,
      iconClass: "bg-muted text-muted-foreground",
    },
  ];

  return (
    <div className="border-b bg-surface-sunken/50 px-3 py-3 sm:px-6">
      <div className="-mx-1 flex gap-2 overflow-x-auto px-1 pb-1 sm:mx-0 sm:grid sm:grid-cols-2 sm:overflow-visible sm:px-0 sm:pb-0 xl:grid-cols-5">
        {cards.map((card) => (
          <div key={card.label} className="min-w-[10rem] workbench-surface px-3 py-2 sm:min-w-0">
            <div className="flex items-start gap-2">
              <span className={`flex h-7 w-7 items-center justify-center rounded-md ${card.iconClass}`}>
                <card.icon className="h-3.5 w-3.5" />
              </span>
              <span className="min-w-0 section-label leading-4">
                {card.label}
              </span>
            </div>
            <div className="mt-1 text-base font-bold tabular-nums tracking-tight sm:text-lg">
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
    { key: "all" as const, label: t("admin.libraries.allReadiness"), count: readinessCounts.all },
    { key: "ready" as const, label: t("admin.libraries.ready"), count: readinessCounts.ready },
    { key: "blocked" as const, label: t("admin.libraries.blocked"), count: readinessCounts.blocked },
  ];
  const lifecycleOptions = [
    { key: "all" as const, label: t("admin.libraries.allLifecycle"), count: lifecycleCounts.all },
    { key: "active" as const, label: t("admin.libraries.activeLifecycle"), count: lifecycleCounts.active },
    { key: "inactive" as const, label: t("admin.libraries.inactiveLifecycle"), count: lifecycleCounts.inactive },
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
      <FilterSelect
        value={workspaceFilter}
        onValueChange={onWorkspaceFilterChange}
        icon={<Building2 />}
        ariaLabel={t("admin.libraries.allWorkspaces")}
        className="w-[200px]"
      >
        <SelectItem value="all">{t("admin.libraries.allWorkspaces")}</SelectItem>
        {workspaces.map((workspace) => (
          <SelectItem key={workspace.id} value={workspace.id}>
            {workspace.displayName}
          </SelectItem>
        ))}
      </FilterSelect>
      <FilterSelect
        value={readinessFilter}
        onValueChange={(value) => onReadinessFilterChange(value as ReadinessFilter)}
        icon={<Database />}
        ariaLabel={t("admin.libraries.allReadiness")}
        className="w-[200px]"
      >
        {readinessOptions.map((option) => (
          <SelectItem key={option.key} value={option.key}>
            <span className="inline-flex items-center gap-2">
              <span>{option.label}</span>
              <span className="tabular-nums text-muted-foreground">{option.count}</span>
            </span>
          </SelectItem>
        ))}
      </FilterSelect>
      <FilterSelect
        value={lifecycleFilter}
        onValueChange={(value) => onLifecycleFilterChange(value as LifecycleFilter)}
        icon={<Power />}
        ariaLabel={t("admin.libraries.allLifecycle")}
        className="w-[200px]"
      >
        {lifecycleOptions.map((option) => (
          <SelectItem key={option.key} value={option.key}>
            <span className="inline-flex items-center gap-2">
              <span>{option.label}</span>
              <span className="tabular-nums text-muted-foreground">{option.count}</span>
            </span>
          </SelectItem>
        ))}
      </FilterSelect>
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

function LibrariesTable({
  allVisibleSelected,
  currencyCode,
  locale,
  onBackup,
  onConfigureAi,
  onDelete,
  onOpenDocuments,
  onRestore,
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
}: LibraryActionHandlers & {
  allVisibleSelected: boolean;
  currencyCode: string;
  locale: string;
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
  const sortIcon = sortDirection === "asc" ? <ArrowUp className="h-3.5 w-3.5" /> : <ArrowDown className="h-3.5 w-3.5" />;

  if (pageRows.length === 0) {
    return <WorkbenchEmptyState title={t("admin.libraries.noMatches")} />;
  }

  return (
    <>
      <div className="space-y-3 p-3 xl:hidden">
        {selectionMode && (
          <label className="workbench-surface flex items-center gap-2 px-3 py-2 text-xs font-semibold text-muted-foreground">
            <Checkbox
              checked={allVisibleSelected}
              aria-label={t("admin.libraries.selectVisible")}
              onCheckedChange={onToggleVisibleSelection}
            />
            {t("admin.libraries.selectVisible")}
          </label>
        )}
        {pageRows.map((row) => (
          <article
            key={row.library.id}
            aria-selected={selectedLibraryId === row.library.id}
            className={`workbench-surface p-4 transition-all ${
              selectedIds.has(row.library.id)
                ? "border-primary/30 bg-primary/10"
                : selectedLibraryId === row.library.id
                  ? "border-primary/40 bg-primary/5"
                  : ""
            }`}
          >
            <div className="flex items-start gap-3">
              {selectionMode && (
                <Checkbox
                  checked={selectedIds.has(row.library.id)}
                  className="mt-1"
                  aria-label={t("admin.libraries.selectLibrary", { name: row.library.displayName })}
                  onCheckedChange={() => onToggleSelection(row.library.id)}
                />
              )}
              <button
                type="button"
                className="min-w-0 flex-1 text-left"
                onClick={() => onSelectRow(row)}
              >
                <div className="flex min-w-0 items-start justify-between gap-3">
                  <LibraryNameCell library={row.library} t={t} />
                  <LifecycleBadge lifecycleState={row.library.lifecycleState} t={t} />
                </div>
                <div className="mt-3 flex flex-wrap items-center gap-2">
                  <ReadinessBadge row={row} t={t} />
                  <span className="rounded-md bg-muted px-2 py-1 text-xs font-semibold text-muted-foreground">
                    {row.workspace.displayName}
                  </span>
                </div>
              </button>
            </div>
            <div className="mt-4 grid grid-cols-3 gap-2 rounded-lg bg-surface-sunken/60 p-2 text-xs">
              <MobileMetric
                label={t("admin.libraries.documents")}
                value={
                  row.costLoading
                    ? t("admin.loading")
                    : formatInteger(row.cost?.documentCount ?? 0, locale)
                }
              />
              <MobileMetric
                label={t("admin.libraries.cost")}
                value={
                  row.costError
                    ? t("admin.libraries.costUnavailable")
                    : formatCurrency(
                        parseCost(row.cost?.totalCost),
                        row.cost?.currencyCode ?? currencyCode,
                        locale,
                      )
                }
              />
              <MobileMetric
                label={t("admin.libraries.calls")}
                value={formatInteger(row.cost?.providerCallCount ?? 0, locale)}
              />
            </div>
            <div className="mt-4 flex justify-end">
              <RowActionsMenu
                actions={libraryRowActions({ onBackup, onConfigureAi, onDelete, onOpenDocuments, onRestore, row, t })}
                className="w-full sm:w-8"
                label={t("admin.libraries.actions")}
              />
            </div>
          </article>
        ))}
      </div>
      <table className="hidden w-full min-w-[1180px] table-fixed text-sm xl:table">
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
        <thead className="sticky top-0 z-10 bg-card">
          <tr className="border-b text-left">
            {selectionMode && (
              <th className="px-4 py-3 w-10">
                <Checkbox
                  checked={allVisibleSelected}
                  aria-label={t("admin.libraries.selectVisible")}
                  onCheckedChange={onToggleVisibleSelection}
                />
              </th>
            )}
            <SortHeader
              active={sortKey === "library"}
              description={t("admin.libraries.columnHelp.library")}
              icon={sortIcon}
              label={t("admin.libraries.library")}
              onClick={() => onToggleSort("library")}
            />
            <SortHeader
              active={sortKey === "workspace"}
              description={t("admin.libraries.columnHelp.workspace")}
              icon={sortIcon}
              label={t("admin.libraries.workspace")}
              onClick={() => onToggleSort("workspace")}
            />
            <SortHeader
              active={sortKey === "documents"}
              description={t("admin.libraries.columnHelp.documents")}
              icon={sortIcon}
              label={t("admin.libraries.documents")}
              onClick={() => onToggleSort("documents")}
            />
            <SortHeader
              active={sortKey === "cost"}
              description={t("admin.libraries.columnHelp.cost")}
              icon={sortIcon}
              label={t("admin.libraries.cost")}
              onClick={() => onToggleSort("cost")}
            />
            <SortHeader
              active={sortKey === "calls"}
              description={t("admin.libraries.columnHelp.calls")}
              icon={sortIcon}
              label={t("admin.libraries.calls")}
              onClick={() => onToggleSort("calls")}
            />
            <SortHeader
              active={sortKey === "readiness"}
              description={t("admin.libraries.columnHelp.readiness")}
              icon={sortIcon}
              label={t("admin.libraries.readiness")}
              onClick={() => onToggleSort("readiness")}
            />
            <SortHeader
              active={sortKey === "lifecycle"}
              description={t("admin.libraries.columnHelp.lifecycle")}
              icon={sortIcon}
              label={t("admin.libraries.lifecycle")}
              onClick={() => onToggleSort("lifecycle")}
            />
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
                <td className="px-4 py-3 w-10">
                  <Checkbox
                    checked={selectedIds.has(row.library.id)}
                    onClick={(event) => event.stopPropagation()}
                    aria-label={t("admin.libraries.selectLibrary", { name: row.library.displayName })}
                    onCheckedChange={() => onToggleSelection(row.library.id)}
                  />
                </td>
              )}
              <td className="px-4 py-3">
                <LibraryNameCell library={row.library} t={t} />
              </td>
              <td className="px-4 py-3">
                <NameWithOptionalSlug
                  displayName={row.workspace.displayName}
                  meta={row.workspace.id}
                  metaTitle={t("admin.libraries.workspaceId")}
                  slug={row.workspace.slug}
                />
              </td>
              <td className="px-4 py-3 text-xs tabular-nums text-muted-foreground">
                {row.costLoading ? (
                  <Loader2 className="h-3.5 w-3.5 animate-spin" />
                ) : (
                  formatInteger(row.cost?.documentCount ?? 0, locale)
                )}
              </td>
              <td className="px-4 py-3 text-xs tabular-nums text-muted-foreground">
                {row.costError
                  ? t("admin.libraries.costUnavailable")
                  : formatCurrency(
                      parseCost(row.cost?.totalCost),
                      row.cost?.currencyCode ?? currencyCode,
                      locale,
                    )}
              </td>
              <td className="px-4 py-3 text-xs tabular-nums text-muted-foreground">
                {formatInteger(row.cost?.providerCallCount ?? 0, locale)}
              </td>
              <td className="px-4 py-3">
                <ReadinessBadge row={row} t={t} />
              </td>
              <td className="px-4 py-3 text-xs text-muted-foreground">
                <LifecycleBadge lifecycleState={row.library.lifecycleState} t={t} />
              </td>
              <td className="px-4 py-3">
                <RowActionsMenu
                  actions={libraryRowActions({ onBackup, onConfigureAi, onDelete, onOpenDocuments, onRestore, row, t })}
                  label={t("admin.libraries.actions")}
                />
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </>
  );
}

function NameWithOptionalSlug({
  displayName,
  meta,
  metaTitle,
  slug,
}: {
  displayName: string;
  meta?: string;
  metaTitle?: string;
  slug: string;
}) {
  const secondarySlug = visibleSecondarySlug(displayName, slug);
  return (
    <div className="min-w-0">
      <span className="block truncate text-sm font-semibold" title={displayName}>
        {displayName}
      </span>
      {secondarySlug && (
        <span className="block truncate font-mono text-2xs text-muted-foreground" title={secondarySlug}>
          {secondarySlug}
        </span>
      )}
      {meta && (
        <span
          className="block truncate font-mono text-2xs text-muted-foreground/80"
          title={metaTitle ? `${metaTitle}: ${meta}` : meta}
        >
          {meta}
        </span>
      )}
    </div>
  );
}

function LibraryNameCell({ library, t }: { library: CatalogLibraryResponse; t: TFunction }) {
  return (
    <div className="flex min-w-0 items-center gap-3">
      <div className="flex h-8 w-8 shrink-0 items-center justify-center rounded-lg bg-surface-sunken">
        <Database className="h-3.5 w-3.5 text-muted-foreground" />
      </div>
      <NameWithOptionalSlug
        displayName={library.displayName}
        meta={library.id}
        metaTitle={t("admin.libraries.libraryId")}
        slug={library.slug}
      />
    </div>
  );
}

function MobileMetric({ label, value }: { label: string; value: string }) {
  return (
    <div className="min-w-0">
      <div className="truncate section-label">
        {label}
      </div>
      <div className="mt-0.5 truncate font-semibold" title={value}>
        {value}
      </div>
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
            <HelpCircle className="h-3.5 w-3.5 text-muted-foreground/70" aria-hidden="true" />
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
            <HelpCircle className="h-3.5 w-3.5 text-muted-foreground/70" aria-hidden="true" />
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

type LibraryActionHandlers = {
  onBackup: (row: LibraryRow) => void;
  onConfigureAi: (row: LibraryRow) => void;
  onDelete: (row: LibraryRow) => void;
  onOpenDocuments: (row: LibraryRow) => void;
  onRestore: (row: LibraryRow) => void;
};

function libraryRowActions({
  onBackup,
  onConfigureAi,
  onDelete,
  onOpenDocuments,
  onRestore,
  row,
  t,
}: LibraryActionHandlers & {
  row: LibraryRow;
  t: TFunction;
}): RowAction[] {
  return [
    {
      key: "documents",
      label: t("admin.libraries.openDocuments"),
      icon: <ExternalLink className="h-3.5 w-3.5" />,
      onSelect: () => onOpenDocuments(row),
    },
    {
      key: "configure-ai",
      label: t("admin.libraries.configureAi"),
      icon: <Brain className="h-3.5 w-3.5" />,
      onSelect: () => onConfigureAi(row),
    },
    {
      key: "backup",
      label: t("admin.snapshot.export"),
      icon: <Download className="h-3.5 w-3.5" />,
      onSelect: () => onBackup(row),
    },
    {
      key: "restore",
      label: t("admin.snapshot.import"),
      icon: <Upload className="h-3.5 w-3.5" />,
      onSelect: () => onRestore(row),
    },
    {
      key: "delete",
      label: t("admin.libraries.delete"),
      icon: <Trash2 className="h-3.5 w-3.5" />,
      onSelect: () => onDelete(row),
      destructive: true,
    },
  ];
}

function ReadinessBadge({ row, t }: { row: LibraryRow; t: TFunction }) {
  const ready = row.library.ingestionReadiness.ready;
  const missingCount = row.library.ingestionReadiness.missingBindingPurposes.length;
  return (
    <span
      title={missingCount > 0 ? row.library.ingestionReadiness.missingBindingPurposes.join(", ") : undefined}
    >
      <StatusBadge tone={ready ? "ready" : "failed"} className="whitespace-nowrap">
        {ready ? <CheckCircle2 className="h-3.5 w-3.5" /> : <XCircle className="h-3.5 w-3.5" />}
        {ready ? t("admin.libraries.ready") : t("admin.libraries.blocked")}
        {!ready && missingCount > 0 ? ` · ${missingCount}` : ""}
      </StatusBadge>
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
    <StatusBadge tone={active ? "ready" : "stalled"} className="whitespace-nowrap">
      {active ? <CheckCircle2 className="h-3.5 w-3.5" /> : <Ban className="h-3.5 w-3.5" />}
      {lifecycleLabel(t, lifecycleState)}
    </StatusBadge>
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
  onBackup,
  onConfigureAi,
  onDelete,
  onOpenDocuments,
  onRestore,
  row,
  t,
}: LibraryActionHandlers & {
  currencyCode: string;
  locale: string;
  row: LibraryRow | null;
  t: TFunction;
}) {
  if (!row) {
    return <InspectorPanel empty={t("admin.libraries.inspectorEmpty")} />;
  }

  return (
    <InspectorPanel
      title={row.library.displayName}
      titleText={row.library.displayName}
      subtitle={row.workspace.displayName}
      metrics={[
        {
          label: t("admin.libraries.documents"),
          value: formatInteger(row.cost?.documentCount ?? 0, locale),
        },
        {
          label: t("admin.libraries.cost"),
          value: formatCurrency(
            parseCost(row.cost?.totalCost),
            row.cost?.currencyCode ?? currencyCode,
            locale,
          ),
        },
        {
          label: t("admin.libraries.calls"),
          value: formatInteger(row.cost?.providerCallCount ?? 0, locale),
        },
        {
          label: t("admin.libraries.lifecycle"),
          value: lifecycleLabel(t, row.library.lifecycleState),
        },
        {
          label: t("admin.libraries.libraryId"),
          value: row.library.id,
          title: row.library.id,
          mono: true,
        },
        {
          label: t("admin.libraries.workspaceId"),
          value: row.workspace.id,
          title: row.workspace.id,
          mono: true,
        },
      ]}
      actions={
        <div className="w-full space-y-2">
          <Button onClick={() => onOpenDocuments(row)} size="sm" className="w-full">
            <ExternalLink className="mr-1.5 h-3.5 w-3.5" />
            {t("admin.libraries.openDocuments")}
          </Button>
          <Button onClick={() => onConfigureAi(row)} size="sm" variant="outline" className="w-full">
            <Brain className="mr-1.5 h-3.5 w-3.5" />
            {t("admin.libraries.configureAi")}
          </Button>
          <div className="grid grid-cols-2 gap-2">
            <Button onClick={() => onBackup(row)} size="sm" variant="outline">
              <Download className="mr-1.5 h-3.5 w-3.5" />
              {t("admin.snapshot.export")}
            </Button>
            <Button onClick={() => onRestore(row)} size="sm" variant="outline">
              <Upload className="mr-1.5 h-3.5 w-3.5" />
              {t("admin.snapshot.import")}
            </Button>
          </div>
          <Button onClick={() => onDelete(row)} size="sm" variant="destructive" className="w-full">
            <Trash2 className="mr-1.5 h-3.5 w-3.5" />
            {t("admin.libraries.delete")}
          </Button>
        </div>
      }
    >
      <LibraryMcpHintToggle library={row.library} t={t} />
    </InspectorPanel>
  );
}

function LibraryMcpHintToggle({ library, t }: { library: CatalogLibraryResponse; t: TFunction }) {
  const { activeLibrary, setActiveLibrary, setLibraries, refreshSession } = useApp();
  const [override, setOverride] = useState<{ id: string; value: boolean } | null>(null);
  const checked =
    override?.id === library.id
      ? override.value
      : library.includeDocumentHintInMcpAnswers ?? true;
  const mutation = useMutation({
    mutationFn: (value: boolean) =>
      adminApi.updateLibraryMcpSettings(library.id, { includeDocumentHintInMcpAnswers: value }),
    onMutate: (value: boolean) => {
      setOverride({ id: library.id, value });
    },
    onSuccess: (updated, requested) => {
      const value = updated.includeDocumentHintInMcpAnswers ?? requested;
      setOverride({ id: library.id, value });
      setLibraries((prev) =>
        prev.map((item) =>
          item.id === library.id ? { ...item, includeDocumentHintInMcpAnswers: value } : item,
        ),
      );
      if (activeLibrary?.id === library.id) {
        setActiveLibrary({ ...activeLibrary, includeDocumentHintInMcpAnswers: value });
      }
      void Promise.resolve(refreshSession()).catch(() => undefined);
    },
    onError: (error: unknown) => {
      setOverride(null);
      toast.error(errorMessage(error, t("admin.mcp.updateFailed")));
    },
  });
  return (
    <label className="flex items-start gap-2.5 rounded-lg bg-surface-sunken/60 p-3">
      <Checkbox
        checked={checked}
        disabled={mutation.isPending}
        className="mt-0.5"
        onCheckedChange={(value) => mutation.mutate(value === true)}
      />
      <span className="min-w-0">
        <span className="block text-xs font-semibold">{t("admin.mcp.includeDocumentHint")}</span>
        <span className="mt-0.5 block text-2xs leading-snug text-muted-foreground">
          {t("admin.mcp.includeDocumentHintHelp")}
        </span>
      </span>
    </label>
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
    <ConfirmDialog
      cancelLabel={t("common.cancel")}
      confirmDisabled={count <= 0}
      confirmLabel={(
        <>
          <Trash2 className="mr-1.5 h-3.5 w-3.5" />
          {t("admin.libraries.delete")}
        </>
      )}
      description={t("admin.libraries.deleteDesc", { count })}
      destructive
      icon={<Trash2 className="h-4 w-4 text-destructive" />}
      onCancel={onCancel}
      onConfirm={onConfirm}
      open={open}
      title={t("admin.libraries.deleteTitle", { count })}
    />
  );
}

function LibrariesLoading({ t }: { t: TFunction }) {
  return (
    <WorkbenchEmptyState
      icon={<Loader2 className="h-7 w-7 animate-spin text-primary/70" />}
      title={t("admin.libraries.loading")}
    />
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
    <WorkbenchEmptyState
      action={(
        <Button onClick={onRetry} size="sm" variant="outline">
          <RotateCw className="mr-1.5 h-3.5 w-3.5" />
          {t("documents.retry")}
        </Button>
      )}
      description={error}
      icon={<XCircle className="h-7 w-7 text-destructive" />}
      title={t("admin.libraries.loadFailed")}
    />
  );
}

function LibrariesEmpty({ t }: { t: TFunction }) {
  return (
    <WorkbenchEmptyState
      icon={<Database className="h-7 w-7 text-muted-foreground" />}
      title={t("admin.libraries.empty")}
    />
  );
}
