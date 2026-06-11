import { useCallback, useMemo, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import type { TFunction } from "i18next";
import {
  ArrowDown,
  ArrowUp,
  CheckSquare,
  Clock3,
  ExternalLink,
  ListOrdered,
  Pause,
  Play,
  RefreshCw,
  Search,
  Square,
} from "lucide-react";
import { useNavigate } from "react-router-dom";
import { toast } from "sonner";
import { adminApi, queries } from "@/shared/api";
import type {
  IngestQueueItemResponse,
  IngestQueueMoveDirection,
  IngestQueueResponse,
  IngestStageEvent,
} from "@/shared/api/generated";
import { useApp } from "@/shared/contexts/app-context";
import { DataState } from "@/shared/components/DataState";
import {
  TABLE_PAGE_SIZE_OPTIONS,
  TablePaginationFooter,
  type TablePageSizeOption,
} from "@/shared/components/TablePaginationFooter";
import { Button } from "@/shared/components/ui/button";
import { Input } from "@/shared/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/shared/components/ui/select";
import { errorMessage } from "@/shared/lib/errorMessage";
import {
  isStorageRecord,
  parseNumberOption,
  useTableState,
} from "@/shared/hooks/useTableState";

type QueueStateFilter = "active" | "running" | "queued" | "paused";
type QueuePageSize = TablePageSizeOption;
type QueueScopeOption = {
  id: string;
  label: string;
  count: number;
};

const ALL_SCOPE_VALUE = "all";

type QueueTableState = {
  pageSize: QueuePageSize;
};

const DEFAULT_QUEUE_TABLE_STATE: QueueTableState = {
  pageSize: 50,
};

type IngestQueueTabProps = {
  t: TFunction;
  active: boolean;
};

function formatQueueTime(value?: string | null): string {
  if (!value) return "—";
  return new Date(value).toLocaleString();
}

function stageLabel(item: IngestQueueItemResponse, t: TFunction): string {
  if (isPausing(item)) {
    return t("admin.queueStatePausing");
  }
  if (item.queueState === "paused") {
    return t("admin.queueStatePaused");
  }
  if (item.queueState === "queued") {
    return t("admin.queueStateQueued");
  }
  return item.currentStage || t("admin.queueStateRunning");
}

function stateBadgeClass(queueState: string): string {
  if (queueState === "leased") return "status-processing";
  if (queueState === "paused") return "status-warning";
  return "bg-muted text-muted-foreground shadow-none";
}

function stateLabel(item: IngestQueueItemResponse, t: TFunction): string {
  if (isPausing(item)) return t("admin.queueStatePausing");
  if (item.queueState === "leased") return t("admin.queueStateRunning");
  if (item.queueState === "paused") return t("admin.queueStatePaused");
  return t("admin.queueStateQueued");
}

function isPausing(item: IngestQueueItemResponse): boolean {
  return (
    item.queueState === "paused" &&
    (item.attemptState === "leased" || item.attemptState === "running")
  );
}

function canMove(item: IngestQueueItemResponse): boolean {
  return item.queueState === "queued" || item.queueState === "paused";
}

function canPause(item: IngestQueueItemResponse): boolean {
  return item.queueState === "queued" || item.queueState === "leased";
}

function canResume(item: IngestQueueItemResponse): boolean {
  return item.queueState === "paused" && !isPausing(item);
}

function progressValue(item: IngestQueueItemResponse): number {
  return Math.max(
    0,
    Math.min(
      100,
      item.progressPercent ?? (item.queueState === "queued" ? 0 : 1),
    ),
  );
}

function eventTone(event: IngestStageEvent): string {
  if (event.stage_state === "failed") return "bg-red-500";
  if (event.stage_state === "completed") return "bg-emerald-500";
  if (event.stage_state === "running" || event.stage_state === "started")
    return "bg-blue-600";
  return "bg-slate-400";
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value && typeof value === "object" && !Array.isArray(value));
}

function formatDetailValue(value: unknown): string {
  if (typeof value === "string") return value;
  if (typeof value === "number" || typeof value === "boolean")
    return String(value);
  if (value == null) return "—";
  return JSON.stringify(value);
}

function stageDetails(event: IngestStageEvent): Array<[string, string]> {
  if (!isRecord(event.details_json)) return [];
  return Object.entries(event.details_json)
    .filter(
      ([, value]) => value !== null && value !== undefined && value !== "",
    )
    .slice(0, 6)
    .map(([key, value]) => [key, formatDetailValue(value)]);
}

function parseQueueTableState(raw: unknown): QueueTableState {
  if (!isStorageRecord(raw)) return DEFAULT_QUEUE_TABLE_STATE;
  return {
    pageSize: parseNumberOption(
      raw.pageSize,
      TABLE_PAGE_SIZE_OPTIONS,
      DEFAULT_QUEUE_TABLE_STATE.pageSize,
    ),
  };
}

function sortScopeOptions(options: QueueScopeOption[]): QueueScopeOption[] {
  return options.sort((first, second) =>
    first.label.localeCompare(second.label, undefined, {
      numeric: true,
      sensitivity: "base",
    }),
  );
}

async function runQueueJobCommand(
  jobIds: string[],
  command: (jobId: string) => Promise<IngestQueueResponse>,
): Promise<IngestQueueResponse> {
  let latestQueue: IngestQueueResponse | null = null;
  for (const jobId of jobIds) {
    latestQueue = await command(jobId);
  }
  if (!latestQueue) {
    throw new Error("No ingest queue jobs selected");
  }
  return latestQueue;
}

export function IngestQueueTab({
  t,
  active,
}: IngestQueueTabProps) {
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const { workspaces, setActiveWorkspace, setActiveLibrary } = useApp();
  const [search, setSearch] = useState("");
  const [stateFilter, setStateFilter] = useState<QueueStateFilter>("active");
  const [workspaceFilter, setWorkspaceFilter] = useState(ALL_SCOPE_VALUE);
  const [libraryFilter, setLibraryFilter] = useState(ALL_SCOPE_VALUE);
  const [selectedJobId, setSelectedJobId] = useState<string | null>(null);
  const [selectionMode, setSelectionMode] = useState(false);
  const [selectedJobIds, setSelectedJobIds] = useState<Set<string>>(
    () => new Set(),
  );
  const [tableState, setTableState] = useTableState<QueueTableState>({
    tableId: "admin.ingestQueue",
    defaultValue: DEFAULT_QUEUE_TABLE_STATE,
    parse: parseQueueTableState,
  });
  const [page, setPage] = useState(1);

  const queueQuery = useQuery({
    ...queries.listIngestQueueOptions(),
    queryFn: () => adminApi.listIngestQueue(),
    enabled: active,
    refetchInterval: active ? 5000 : false,
  });
  const {
    data: queueData,
    error: queueError,
    isFetching: queueIsFetching,
    isLoading: queueIsLoading,
    refetch: refetchQueue,
  } = queueQuery;

  const refreshQueue = useCallback(() => {
    void refetchQueue();
  }, [refetchQueue]);

  const applyQueue = useCallback(
    (queue: IngestQueueResponse) => {
      queryClient.setQueryData(queries.listIngestQueueQueryKey(), queue);
    },
    [queryClient],
  );

  const moveMutation = useMutation({
    mutationFn: ({
      jobId,
      direction,
    }: {
      jobId: string;
      direction: IngestQueueMoveDirection;
    }) => adminApi.moveIngestQueueJob(jobId, direction),
    onSuccess: applyQueue,
    onError: (error) => {
      toast.error(errorMessage(error, t("admin.queueMoveFailed")));
    },
  });

  const cancelMutation = useMutation({
    mutationFn: (jobId: string) => adminApi.cancelIngestQueueJob(jobId),
    onSuccess: applyQueue,
    onError: (error) => {
      toast.error(errorMessage(error, t("admin.queueCancelFailed")));
    },
  });

  const pauseMutation = useMutation({
    mutationFn: (jobId: string) => adminApi.pauseIngestQueueJob(jobId),
    onSuccess: applyQueue,
    onError: (error) => {
      toast.error(errorMessage(error, t("admin.queuePauseFailed")));
    },
  });

  const resumeMutation = useMutation({
    mutationFn: (jobId: string) => adminApi.resumeIngestQueueJob(jobId),
    onSuccess: applyQueue,
    onError: (error) => {
      toast.error(errorMessage(error, t("admin.queueResumeFailed")));
    },
  });

  const bulkCancelMutation = useMutation({
    mutationFn: (jobIds: string[]) =>
      runQueueJobCommand(jobIds, (jobId) => adminApi.cancelIngestQueueJob(jobId)),
    onSuccess: (queue) => {
      applyQueue(queue);
      setSelectedJobIds(new Set());
      setSelectionMode(false);
      toast.success(
        t("admin.queueBulkCancelSuccess", { count: selectedJobIds.size }),
      );
    },
    onError: (error) => {
      toast.error(errorMessage(error, t("admin.queueBulkCancelFailed")));
    },
  });

  const bulkPauseMutation = useMutation({
    mutationFn: (jobIds: string[]) =>
      runQueueJobCommand(jobIds, (jobId) => adminApi.pauseIngestQueueJob(jobId)),
    onSuccess: (queue) => {
      applyQueue(queue);
      setSelectedJobIds(new Set());
      toast.success(
        t("admin.queueBulkPauseSuccess", { count: selectedJobIds.size }),
      );
    },
    onError: (error) => {
      toast.error(errorMessage(error, t("admin.queueBulkPauseFailed")));
    },
  });

  const bulkResumeMutation = useMutation({
    mutationFn: (jobIds: string[]) =>
      runQueueJobCommand(jobIds, (jobId) => adminApi.resumeIngestQueueJob(jobId)),
    onSuccess: (queue) => {
      applyQueue(queue);
      setSelectedJobIds(new Set());
      toast.success(
        t("admin.queueBulkResumeSuccess", { count: selectedJobIds.size }),
      );
    },
    onError: (error) => {
      toast.error(errorMessage(error, t("admin.queueBulkResumeFailed")));
    },
  });

  const workspaceOptions = useMemo(() => {
    const byId = new Map<string, QueueScopeOption>();
    for (const item of queueData?.items ?? []) {
      const current = byId.get(item.workspaceId);
      if (current) {
        current.count += 1;
      } else {
        byId.set(item.workspaceId, {
          id: item.workspaceId,
          label: item.workspaceName,
          count: 1,
        });
      }
    }
    return sortScopeOptions(Array.from(byId.values()));
  }, [queueData?.items]);
  const effectiveWorkspaceFilter =
    workspaceFilter === ALL_SCOPE_VALUE ||
    workspaceOptions.some((option) => option.id === workspaceFilter)
      ? workspaceFilter
      : ALL_SCOPE_VALUE;

  const libraryOptions = useMemo(() => {
    const byId = new Map<string, QueueScopeOption>();
    for (const item of queueData?.items ?? []) {
      if (
        effectiveWorkspaceFilter !== ALL_SCOPE_VALUE &&
        item.workspaceId !== effectiveWorkspaceFilter
      ) {
        continue;
      }
      const current = byId.get(item.libraryId);
      if (current) {
        current.count += 1;
      } else {
        byId.set(item.libraryId, {
          id: item.libraryId,
          label:
            effectiveWorkspaceFilter === ALL_SCOPE_VALUE
              ? `${item.libraryName} · ${item.workspaceName}`
              : item.libraryName,
          count: 1,
        });
      }
    }
    return sortScopeOptions(Array.from(byId.values()));
  }, [effectiveWorkspaceFilter, queueData?.items]);
  const effectiveLibraryFilter =
    libraryFilter === ALL_SCOPE_VALUE ||
    libraryOptions.some((option) => option.id === libraryFilter)
      ? libraryFilter
      : ALL_SCOPE_VALUE;

  const filteredItems = useMemo(() => {
    const needle = search.trim().toLowerCase();
    return (queueData?.items ?? []).filter((item) => {
      if (
        effectiveWorkspaceFilter !== ALL_SCOPE_VALUE &&
        item.workspaceId !== effectiveWorkspaceFilter
      ) {
        return false;
      }
      if (
        effectiveLibraryFilter !== ALL_SCOPE_VALUE &&
        item.libraryId !== effectiveLibraryFilter
      ) {
        return false;
      }
      if (stateFilter === "running" && item.queueState !== "leased")
        return false;
      if (stateFilter === "queued" && item.queueState !== "queued")
        return false;
      if (stateFilter === "paused" && item.queueState !== "paused")
        return false;
      if (!needle) return true;
      return [
        item.documentName,
        item.workspaceName,
        item.libraryName,
        item.currentStage,
        item.jobKind,
      ].some((value) => value?.toLowerCase().includes(needle));
    });
  }, [
    effectiveLibraryFilter,
    effectiveWorkspaceFilter,
    queueData?.items,
    search,
    stateFilter,
  ]);

  const pageSize = tableState.pageSize;
  const totalPages = Math.max(1, Math.ceil(filteredItems.length / pageSize));
  const currentPage = Math.min(page, totalPages);
  const visibleStart =
    filteredItems.length === 0 ? 0 : (currentPage - 1) * pageSize + 1;
  const visibleEnd = Math.min(currentPage * pageSize, filteredItems.length);
  const pagedItems = useMemo(
    () =>
      filteredItems.slice((currentPage - 1) * pageSize, currentPage * pageSize),
    [currentPage, filteredItems, pageSize],
  );
  const selectedItems = useMemo(
    () => filteredItems.filter((item) => selectedJobIds.has(item.jobId)),
    [filteredItems, selectedJobIds],
  );
  const pausableSelectedItems = selectedItems.filter(canPause);
  const resumableSelectedItems = selectedItems.filter(canResume);
  const allVisibleSelected =
    pagedItems.length > 0 &&
    pagedItems.every((item) => selectedJobIds.has(item.jobId));

  const selectedItem = useMemo(() => {
    return (
      pagedItems.find((item) => item.jobId === selectedJobId) ??
      pagedItems[0] ??
      null
    );
  }, [pagedItems, selectedJobId]);

  const timelineQuery = useQuery({
    ...queries.listIngestStageEventsOptions({
      path: { attemptId: selectedItem?.attemptId ?? "" },
    }),
    enabled: active && Boolean(selectedItem?.attemptId),
    refetchInterval:
      active &&
      (selectedItem?.queueState === "leased" ||
        (selectedItem && isPausing(selectedItem)))
        ? 3000
        : false,
  });

  const openDocuments = useCallback(
    (item: IngestQueueItemResponse) => {
      const workspace = workspaces.find(
        (candidate) => candidate.id === item.workspaceId,
      ) ?? {
        id: item.workspaceId,
        name: item.workspaceName,
        createdAt: "",
      };
      setActiveWorkspace(workspace);
      setActiveLibrary({
        id: item.libraryId,
        workspaceId: item.workspaceId,
        name: item.libraryName,
        createdAt: "",
        ingestionReady: true,
        queryReady: true,
        missingBindingPurposes: [],
      });
      const params = new URLSearchParams();
      if (item.documentId) params.set("documentId", item.documentId);
      void navigate(
        `/documents${params.size > 0 ? `?${params.toString()}` : ""}`,
      );
    },
    [navigate, setActiveLibrary, setActiveWorkspace, workspaces],
  );

  const movingJobId = moveMutation.variables?.jobId;
  const cancelingJobId = cancelMutation.variables;
  const pausingJobId = pauseMutation.variables;
  const resumingJobId = resumeMutation.variables;
  const bulkActionPending =
    bulkCancelMutation.isPending || bulkPauseMutation.isPending || bulkResumeMutation.isPending;

  const cancelSelection = () => {
    setSelectionMode(false);
    setSelectedJobIds(new Set());
  };

  const toggleVisibleSelection = () => {
    setSelectedJobIds((current) => {
      const next = new Set(current);
      if (allVisibleSelected) {
        pagedItems.forEach((item) => next.delete(item.jobId));
      } else {
        pagedItems.forEach((item) => next.add(item.jobId));
      }
      return next;
    });
  };

  const toggleJobSelection = (jobId: string) => {
    setSelectedJobIds((current) => {
      const next = new Set(current);
      if (next.has(jobId)) {
        next.delete(jobId);
      } else {
        next.add(jobId);
      }
      return next;
    });
  };

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div
        className="mb-4 flex shrink-0 flex-col gap-3 xl:flex-row xl:items-center xl:justify-between"
      >
        <div>
          <h2 className="flex items-center gap-2 text-base font-bold tracking-tight">
            <ListOrdered className="h-4 w-4 text-muted-foreground" />
            {t("admin.ingestQueue")}
          </h2>
          <p className="mt-1 text-xs text-muted-foreground">
            {t("admin.ingestQueueDesc")}
          </p>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <div className="rounded-lg border bg-card px-3 py-2 text-xs">
            <span className="text-muted-foreground">
              {t("admin.queueRunning")}
            </span>
            <span className="ml-2 font-bold tabular-nums">
              {queueData?.summary.running ?? 0}
            </span>
          </div>
          <div className="rounded-lg border bg-card px-3 py-2 text-xs">
            <span className="text-muted-foreground">
              {t("admin.queueQueued")}
            </span>
            <span className="ml-2 font-bold tabular-nums">
              {queueData?.summary.queued ?? 0}
            </span>
          </div>
          <div className="rounded-lg border bg-card px-3 py-2 text-xs">
            <span className="text-muted-foreground">
              {t("admin.queuePaused")}
            </span>
            <span className="ml-2 font-bold tabular-nums">
              {queueData?.summary.paused ?? 0}
            </span>
          </div>
          <Button size="sm" variant="outline" onClick={refreshQueue}>
            <RefreshCw
              className={`mr-1.5 h-3.5 w-3.5 ${queueIsFetching ? "animate-spin" : ""}`}
            />
            {t("dashboard.refresh")}
          </Button>
        </div>
      </div>

      <div className="flex min-h-0 flex-1 overflow-hidden">
        <div className="flex min-w-0 flex-1 flex-col">
          <div className="mb-3 grid shrink-0 grid-cols-1 gap-2 lg:grid-cols-[minmax(220px,1.3fr)_minmax(180px,0.8fr)_minmax(200px,0.9fr)_minmax(220px,1fr)_auto] lg:items-center">
            <div className="relative min-w-0">
              <Search className="absolute left-3 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
              <Input
                className="h-8 pl-9 text-xs"
                placeholder={t("admin.queueSearchPlaceholder")}
                value={search}
                onChange={(event) => {
                  setSearch(event.target.value);
                  setPage(1);
                  setSelectedJobId(null);
                  setSelectedJobIds(new Set());
                }}
              />
            </div>
            <Select
              value={effectiveWorkspaceFilter}
              onValueChange={(value) => {
                setWorkspaceFilter(value);
                setLibraryFilter(ALL_SCOPE_VALUE);
                setPage(1);
                setSelectedJobId(null);
                setSelectedJobIds(new Set());
              }}
            >
              <SelectTrigger
                aria-label={t("admin.queueWorkspaceFilter")}
                className="h-8 w-full text-xs"
              >
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value={ALL_SCOPE_VALUE}>
                  {t("admin.queueAllWorkspaces", {
                    count: queueData?.items.length ?? 0,
                  })}
                </SelectItem>
                {workspaceOptions.map((option) => (
                  <SelectItem key={option.id} value={option.id}>
                    {t("admin.queueFilterOptionWithCount", {
                      count: option.count,
                      name: option.label,
                    })}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            <Select
              value={effectiveLibraryFilter}
              onValueChange={(value) => {
                setLibraryFilter(value);
                setPage(1);
                setSelectedJobId(null);
                setSelectedJobIds(new Set());
              }}
            >
              <SelectTrigger
                aria-label={t("admin.queueLibraryFilter")}
                className="h-8 w-full text-xs"
              >
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value={ALL_SCOPE_VALUE}>
                  {t("admin.queueAllLibraries", {
                    count: libraryOptions.reduce(
                      (total, option) => total + option.count,
                      0,
                    ),
                  })}
                </SelectItem>
                {libraryOptions.map((option) => (
                  <SelectItem key={option.id} value={option.id}>
                    {t("admin.queueFilterOptionWithCount", {
                      count: option.count,
                      name: option.label,
                    })}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            <Select
              value={stateFilter}
              onValueChange={(value) => {
                setStateFilter(value as QueueStateFilter);
                setPage(1);
                setSelectedJobId(null);
                setSelectedJobIds(new Set());
              }}
            >
              <SelectTrigger className="h-8 w-full text-xs">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="active">
                  {t("admin.queueFilterActive")}
                </SelectItem>
                <SelectItem value="running">
                  {t("admin.queueFilterRunning")}
                </SelectItem>
                <SelectItem value="queued">
                  {t("admin.queueFilterQueued")}
                </SelectItem>
                <SelectItem value="paused">
                  {t("admin.queueFilterPaused")}
                </SelectItem>
              </SelectContent>
            </Select>
            <Button
              className="h-8 text-xs lg:justify-self-end"
              onClick={selectionMode ? cancelSelection : () => setSelectionMode(true)}
              size="sm"
              variant={selectionMode ? "default" : "outline"}
            >
              <CheckSquare className="mr-1.5 h-3.5 w-3.5" />
              {selectionMode ? t("admin.queueCancelSelection") : t("admin.queueSelect")}
            </Button>
          </div>

          <div className="min-h-0 flex-1 overflow-hidden">
            <DataState
              query={{
                isLoading: queueIsLoading && active,
                error: queueError
                  ? errorMessage(queueError, t("admin.loadQueueFailed"))
                  : null,
                data: queueData,
              }}
              emptyCheck={(queue) => (queue.items ?? []).length === 0}
              emptyRender={
                <div className="flex min-h-64 items-center justify-center rounded-xl border bg-card text-sm text-muted-foreground">
                  {t("admin.queueEmpty")}
                </div>
              }
            >
              {() => (
                <div className="flex h-full min-h-0 flex-col">
                  <div className="min-h-0 flex-1 overflow-auto workbench-surface rounded-b-none">
                    <table className="w-full min-w-[1000px] table-fixed text-sm">
                      <colgroup>
                        {selectionMode && <col className="w-[4%]" />}
                        <col className="w-[7%]" />
                        <col className={selectionMode ? "w-[22%]" : "w-[25%]"} />
                        <col className="w-[17%]" />
                        <col className="w-[12%]" />
                        <col className="w-[14%]" />
                        <col className="w-[13%]" />
                        <col className="w-[12%]" />
                      </colgroup>
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
                            <th className="px-4 py-3">
                              <input
                                aria-label={t("admin.queueSelectVisible")}
                                checked={allVisibleSelected}
                                className="h-4 w-4 rounded border-gray-300"
                                onChange={toggleVisibleSelection}
                                type="checkbox"
                              />
                            </th>
                          )}
                          <th className="px-4 py-3 section-label">
                            {t("admin.queueOrder")}
                          </th>
                          <th className="px-4 py-3 section-label">
                            {t("documents.name")}
                          </th>
                          <th className="px-4 py-3 section-label">
                            {t("admin.scope")}
                          </th>
                          <th className="px-4 py-3 section-label">
                            {t("admin.status")}
                          </th>
                          <th className="px-4 py-3 section-label">
                            {t("admin.queueStage")}
                          </th>
                          <th className="px-4 py-3 section-label">
                            {t("admin.queueQueuedAt")}
                          </th>
                          <th className="px-4 py-3 section-label text-right">
                            {t("admin.queueActions")}
                          </th>
                        </tr>
                      </thead>
                      <tbody>
                        {pagedItems.map((item) => {
                          const selected = selectedItem?.jobId === item.jobId;
                          return (
                            <tr
                              key={item.jobId}
                              className={`cursor-pointer border-b border-border/50 transition-colors ${
                                selected
                                  ? "border-l-2 border-l-primary bg-primary/5"
                                  : "hover:bg-accent/30"
                              }`}
                              onClick={() => setSelectedJobId(item.jobId)}
                            >
                              {selectionMode && (
                                <td className="px-4 py-3">
                                  <input
                                    aria-label={t("admin.queueSelectJob", { name: item.documentName })}
                                    checked={selectedJobIds.has(item.jobId)}
                                    className="h-4 w-4 rounded border-gray-300"
                                    onChange={(event) => {
                                      event.stopPropagation();
                                      toggleJobSelection(item.jobId);
                                    }}
                                    onClick={(event) => event.stopPropagation()}
                                    type="checkbox"
                                  />
                                </td>
                              )}
                              <td className="px-4 py-3 font-mono text-xs text-muted-foreground">
                                {item.queueState === "leased"
                                  ? t("admin.queueNow")
                                  : `#${item.queuePosition ?? "—"}`}
                              </td>
                              <td className="px-4 py-3">
                                <div
                                  className="max-w-md truncate font-semibold"
                                  title={item.documentName}
                                >
                                  {item.documentName}
                                </div>
                                <div className="mt-1 text-[11px] text-muted-foreground">
                                  {item.jobKind}
                                </div>
                              </td>
                              <td className="px-4 py-3 text-xs">
                                <div className="font-semibold">
                                  {item.libraryName}
                                </div>
                                <div className="mt-1 text-muted-foreground">
                                  {item.workspaceName}
                                </div>
                              </td>
                              <td className="px-4 py-3">
                                <span
                                  className={`status-badge text-[10px] ${stateBadgeClass(item.queueState)}`}
                                >
                                  {stateLabel(item, t)}
                                </span>
                              </td>
                              <td className="px-4 py-3 text-xs">
                                <div className="font-medium">
                                  {stageLabel(item, t)}
                                </div>
                                <div className="mt-1 text-muted-foreground">
                                  {isPausing(item)
                                    ? t("admin.queuePausingWaiting")
                                    : item.queueState === "paused"
                                      ? t("admin.queuePausedWaiting")
                                      : item.progressPercent != null
                                        ? t("admin.queueProgressValue", {
                                            value: item.progressPercent,
                                          })
                                        : item.attemptNumber
                                          ? t("admin.queueAttemptValue", {
                                              value: item.attemptNumber,
                                            })
                                          : t("admin.queueWaiting")}
                                </div>
                              </td>
                              <td className="px-4 py-3 text-xs text-muted-foreground">
                                <div>{formatQueueTime(item.queuedAt)}</div>
                                {item.startedAt && (
                                  <div className="mt-1">
                                    {t("admin.queueStartedAt", {
                                      value: formatQueueTime(item.startedAt),
                                    })}
                                  </div>
                                )}
                              </td>
                              <td className="px-4 py-3">
                                <div className="flex items-center justify-end gap-1">
                                  <Button
                                    type="button"
                                    size="icon"
                                    variant="ghost"
                                    className="h-8 w-8"
                                    disabled={
                                      !canMove(item) || moveMutation.isPending
                                    }
                                    onClick={(event) => {
                                      event.stopPropagation();
                                      moveMutation.mutate({
                                        jobId: item.jobId,
                                        direction: "up",
                                      });
                                    }}
                                    title={t("admin.queueMoveUp")}
                                  >
                                    <ArrowUp
                                      className={`h-4 w-4 ${movingJobId === item.jobId ? "animate-pulse" : ""}`}
                                    />
                                  </Button>
                                  <Button
                                    type="button"
                                    size="icon"
                                    variant="ghost"
                                    className="h-8 w-8"
                                    disabled={
                                      !canMove(item) || moveMutation.isPending
                                    }
                                    onClick={(event) => {
                                      event.stopPropagation();
                                      moveMutation.mutate({
                                        jobId: item.jobId,
                                        direction: "down",
                                      });
                                    }}
                                    title={t("admin.queueMoveDown")}
                                  >
                                    <ArrowDown
                                      className={`h-4 w-4 ${movingJobId === item.jobId ? "animate-pulse" : ""}`}
                                    />
                                  </Button>
                                  {item.queueState === "paused" ? (
                                    <Button
                                      type="button"
                                      size="icon"
                                      variant="ghost"
                                      className="h-8 w-8 text-status-ready hover:text-status-ready"
                                      disabled={
                                        !canResume(item) ||
                                        resumeMutation.isPending
                                      }
                                      onClick={(event) => {
                                        event.stopPropagation();
                                        resumeMutation.mutate(item.jobId);
                                      }}
                                      title={
                                        isPausing(item)
                                          ? t("admin.queueResumeBlocked")
                                          : t("admin.queueResumeJob")
                                      }
                                    >
                                      <Play
                                        className={`h-4 w-4 ${resumingJobId === item.jobId ? "animate-pulse" : ""}`}
                                      />
                                    </Button>
                                  ) : (
                                    <Button
                                      type="button"
                                      size="icon"
                                      variant="ghost"
                                      className="h-8 w-8 text-status-warning hover:text-status-warning"
                                      disabled={
                                        !canPause(item) ||
                                        pauseMutation.isPending
                                      }
                                      onClick={(event) => {
                                        event.stopPropagation();
                                        pauseMutation.mutate(item.jobId);
                                      }}
                                      title={t("admin.queuePauseJob")}
                                    >
                                      <Pause
                                        className={`h-4 w-4 ${pausingJobId === item.jobId ? "animate-pulse" : ""}`}
                                      />
                                    </Button>
                                  )}
                                  <Button
                                    type="button"
                                    size="icon"
                                    variant="ghost"
                                    className="h-8 w-8"
                                    onClick={(event) => {
                                      event.stopPropagation();
                                      openDocuments(item);
                                    }}
                                    title={t("admin.queueOpenDocuments")}
                                  >
                                    <ExternalLink className="h-4 w-4" />
                                  </Button>
                                  <Button
                                    type="button"
                                    size="icon"
                                    variant="ghost"
                                    className="h-8 w-8 text-status-failed hover:text-status-failed"
                                    disabled={cancelMutation.isPending}
                                    onClick={(event) => {
                                      event.stopPropagation();
                                      cancelMutation.mutate(item.jobId);
                                    }}
                                    title={t("admin.queueCancelJob")}
                                  >
                                    <Square
                                      className={`h-4 w-4 ${cancelingJobId === item.jobId ? "animate-pulse" : ""}`}
                                    />
                                  </Button>
                                </div>
                              </td>
                            </tr>
                          );
                        })}
                      </tbody>
                    </table>
                    {filteredItems.length === 0 && (
                      <div className="py-16 text-center text-sm text-muted-foreground">
                        {t("admin.queueNoMatches")}
                      </div>
                    )}
                  </div>
                  {filteredItems.length > 0 && (
                    <>
                      <QueueBulkBar
                        bulkActionPending={bulkActionPending}
                        cancelCount={selectedItems.length}
                        onCancel={() =>
                          bulkCancelMutation.mutate(
                            selectedItems.map((item) => item.jobId),
                          )
                        }
                        onClear={() => setSelectedJobIds(new Set())}
                        onPause={() =>
                          bulkPauseMutation.mutate(
                            pausableSelectedItems.map((item) => item.jobId),
                          )
                        }
                        onResume={() =>
                          bulkResumeMutation.mutate(
                            resumableSelectedItems.map((item) => item.jobId),
                          )
                        }
                        pauseCount={pausableSelectedItems.length}
                        resumeCount={resumableSelectedItems.length}
                        selectedCount={selectedItems.length}
                        t={t}
                      />
                      <QueuePaginationFooter
                        currentPage={currentPage}
                        pageSize={pageSize}
                        t={t}
                        totalItems={filteredItems.length}
                        totalPages={totalPages}
                        visibleEnd={visibleEnd}
                        visibleStart={visibleStart}
                        onPageSizeChange={(nextPageSize) => {
                          setTableState({ pageSize: nextPageSize });
                          setPage(1);
                          setSelectedJobId(null);
                          setSelectedJobIds(new Set());
                        }}
                        onGoToPage={(target) => {
                          setPage(target);
                          setSelectedJobId(null);
                          setSelectedJobIds(new Set());
                        }}
                      />
                    </>
                  )}
                </div>
              )}
            </DataState>
          </div>
        </div>

        <aside className="inspector-panel hidden w-80 shrink-0 animate-slide-in-right overflow-y-auto md:block lg:w-96">
          {selectedItem ? (
            <div className="space-y-4 p-4">
              <div className="flex items-start justify-between gap-3 border-b border-border/70 pb-3">
                <div className="min-w-0">
                  <div className="section-label">
                    {t("admin.queueInspectorTitle")}
                  </div>
                  <h3
                    className="mt-1 truncate text-sm font-bold"
                    title={selectedItem.documentName}
                  >
                    {selectedItem.documentName}
                  </h3>
                </div>
                <span
                  className={`status-badge shrink-0 text-[10px] ${stateBadgeClass(selectedItem.queueState)}`}
                >
                  {stateLabel(selectedItem, t)}
                </span>
              </div>

              <div>
                <div className="mb-1 flex items-center justify-between text-xs">
                  <span className="font-semibold">
                    {t("admin.queueInspectorProgress")}
                  </span>
                  <span className="font-mono">
                    {progressValue(selectedItem)}%
                  </span>
                </div>
                <div className="h-2 rounded-full bg-muted">
                  <div
                    className="h-full rounded-full bg-primary transition-all"
                    style={{ width: `${progressValue(selectedItem)}%` }}
                  />
                </div>
              </div>

              <dl className="grid grid-cols-2 gap-3 text-xs">
                <div>
                  <dt className="text-muted-foreground">
                    {t("admin.queueInspectorScope")}
                  </dt>
                  <dd className="mt-0.5 font-semibold">
                    {selectedItem.libraryName}
                  </dd>
                  <dd className="truncate text-muted-foreground">
                    {selectedItem.workspaceName}
                  </dd>
                </div>
                <div>
                  <dt className="text-muted-foreground">
                    {t("admin.queueStage")}
                  </dt>
                  <dd className="mt-0.5 font-semibold">
                    {stageLabel(selectedItem, t)}
                  </dd>
                  <dd className="truncate text-muted-foreground">
                    {selectedItem.jobKind}
                  </dd>
                </div>
                <div>
                  <dt className="text-muted-foreground">
                    {t("admin.queueQueuedAt")}
                  </dt>
                  <dd className="mt-0.5 font-semibold">
                    {formatQueueTime(selectedItem.queuedAt)}
                  </dd>
                </div>
                <div>
                  <dt className="text-muted-foreground">
                    {t("admin.queueInspectorHeartbeat")}
                  </dt>
                  <dd className="mt-0.5 font-semibold">
                    {formatQueueTime(selectedItem.heartbeatAt)}
                  </dd>
                </div>
              </dl>

              <div className="flex flex-wrap gap-2">
                {selectedItem.queueState === "paused" ? (
                  <Button
                    size="sm"
                    variant="outline"
                    className="text-status-ready hover:text-status-ready"
                    disabled={
                      !canResume(selectedItem) || resumeMutation.isPending
                    }
                    onClick={() => resumeMutation.mutate(selectedItem.jobId)}
                    title={
                      isPausing(selectedItem)
                        ? t("admin.queueResumeBlocked")
                        : t("admin.queueResumeJob")
                    }
                  >
                    <Play className="mr-1.5 h-3.5 w-3.5" />
                    {t("admin.queueResumeJob")}
                  </Button>
                ) : (
                  <Button
                    size="sm"
                    variant="outline"
                    className="text-status-warning hover:text-status-warning"
                    disabled={
                      !canPause(selectedItem) || pauseMutation.isPending
                    }
                    onClick={() => pauseMutation.mutate(selectedItem.jobId)}
                  >
                    <Pause className="mr-1.5 h-3.5 w-3.5" />
                    {t("admin.queuePauseJob")}
                  </Button>
                )}
                <Button
                  size="sm"
                  variant="outline"
                  onClick={() => openDocuments(selectedItem)}
                >
                  <ExternalLink className="mr-1.5 h-3.5 w-3.5" />
                  {t("admin.queueOpenDocuments")}
                </Button>
                <Button
                  size="sm"
                  variant="outline"
                  className="text-status-failed hover:text-status-failed"
                  disabled={cancelMutation.isPending}
                  onClick={() => cancelMutation.mutate(selectedItem.jobId)}
                >
                  <Square className="mr-1.5 h-3.5 w-3.5" />
                  {t("admin.queueCancelJob")}
                </Button>
              </div>

              {selectedItem.failureMessage && (
                <div
                  className={`rounded-lg border px-3 py-2 text-xs ${
                    selectedItem.queueState === "paused"
                      ? "border-status-warning/30 bg-status-warning/5 text-status-warning"
                      : "border-red-200 bg-red-50 text-red-700"
                  }`}
                >
                  <div className="font-bold">
                    {selectedItem.queueState === "paused"
                      ? t("admin.queueStatePaused")
                      : (selectedItem.failureCode ?? t("admin.queueFailure"))}
                  </div>
                  <div className="mt-1 whitespace-pre-wrap">
                    {selectedItem.failureMessage}
                  </div>
                </div>
              )}

              <div>
                <div className="mb-2 flex items-center gap-2 section-label">
                  <Clock3 className="h-3.5 w-3.5" />
                  {t("admin.queueInspectorTimeline")}
                </div>
                {!selectedItem.attemptId ? (
                  <div className="rounded-lg border bg-muted/30 px-3 py-4 text-sm text-muted-foreground">
                    {t("admin.queueInspectorNoAttempt")}
                  </div>
                ) : timelineQuery.isLoading ? (
                  <div className="rounded-lg border bg-muted/30 px-3 py-4 text-sm text-muted-foreground">
                    {t("admin.queueInspectorLoading")}
                  </div>
                ) : timelineQuery.error ? (
                  <div className="rounded-lg border bg-red-50 px-3 py-4 text-sm text-red-700">
                    {errorMessage(
                      timelineQuery.error,
                      t("admin.queueInspectorError"),
                    )}
                  </div>
                ) : (timelineQuery.data?.stages?.length ?? 0) === 0 ? (
                  <div className="rounded-lg border bg-muted/30 px-3 py-4 text-sm text-muted-foreground">
                    {t("admin.queueInspectorNoEvents")}
                  </div>
                ) : (
                  <div className="overflow-hidden rounded-lg border">
                    {timelineQuery.data?.stages.map((event) => (
                      <div
                        key={event.id}
                        className="border-b p-3 last:border-b-0"
                      >
                        <div className="flex items-start justify-between gap-3">
                          <div className="min-w-0">
                            <div className="flex items-center gap-2">
                              <span
                                className={`h-2 w-2 shrink-0 rounded-full ${eventTone(event)}`}
                              />
                              <span className="truncate text-sm font-semibold">
                                {event.stage_name}
                              </span>
                            </div>
                            {event.message && (
                              <p className="mt-1 whitespace-pre-wrap text-xs text-muted-foreground">
                                {event.message}
                              </p>
                            )}
                          </div>
                          <div className="shrink-0 text-right text-[11px] text-muted-foreground">
                            <div>{event.stage_state}</div>
                            <div>{formatQueueTime(event.recorded_at)}</div>
                          </div>
                        </div>
                        {stageDetails(event).length > 0 && (
                          <div className="mt-2 flex flex-wrap gap-1.5">
                            {stageDetails(event).map(([key, value]) => (
                              <span
                                key={key}
                                className="rounded-md bg-muted px-2 py-1 text-[11px]"
                              >
                                <span className="text-muted-foreground">
                                  {key}
                                </span>{" "}
                                <span className="font-semibold">{value}</span>
                              </span>
                            ))}
                          </div>
                        )}
                      </div>
                    ))}
                  </div>
                )}
              </div>
            </div>
          ) : (
            <div className="flex h-full min-h-64 items-center justify-center text-center text-sm text-muted-foreground">
              {t("admin.queueInspectorEmpty")}
            </div>
          )}
        </aside>
      </div>
    </div>
  );
}

type QueuePaginationFooterProps = {
  currentPage: number;
  onGoToPage: (page: number) => void;
  onPageSizeChange: (pageSize: QueuePageSize) => void;
  pageSize: QueuePageSize;
  t: TFunction;
  totalItems: number;
  totalPages: number;
  visibleEnd: number;
  visibleStart: number;
};

type QueueBulkBarProps = {
  bulkActionPending: boolean;
  cancelCount: number;
  onCancel: () => void;
  onClear: () => void;
  onPause: () => void;
  onResume: () => void;
  pauseCount: number;
  resumeCount: number;
  selectedCount: number;
  t: TFunction;
};

function QueueBulkBar({
  bulkActionPending,
  cancelCount,
  onCancel,
  onClear,
  onPause,
  onResume,
  pauseCount,
  resumeCount,
  selectedCount,
  t,
}: QueueBulkBarProps) {
  if (selectedCount <= 0) return null;
  return (
    <div className="shrink-0 border-t bg-primary/5 px-4 py-2">
      <div className="flex flex-wrap items-center gap-2">
        <span className="mr-auto text-xs font-semibold text-primary tabular-nums">
          {t("admin.queueSelected", { count: selectedCount })}
        </span>
        <Button
          className="h-8 text-xs"
          disabled={pauseCount <= 0 || bulkActionPending}
          onClick={onPause}
          size="sm"
          variant="outline"
        >
          <Pause className="mr-1.5 h-3.5 w-3.5" />
          {t("admin.queuePauseSelected", { count: pauseCount })}
        </Button>
        <Button
          className="h-8 text-xs"
          disabled={resumeCount <= 0 || bulkActionPending}
          onClick={onResume}
          size="sm"
          variant="outline"
        >
          <Play className="mr-1.5 h-3.5 w-3.5" />
          {t("admin.queueResumeSelected", { count: resumeCount })}
        </Button>
        <Button
          className="h-8 text-xs text-status-failed hover:text-status-failed"
          disabled={cancelCount <= 0 || bulkActionPending}
          onClick={onCancel}
          size="sm"
          variant="outline"
        >
          <Square className="mr-1.5 h-3.5 w-3.5" />
          {t("admin.queueCancelSelected", { count: cancelCount })}
        </Button>
        <Button
          className="h-8 text-xs"
          disabled={bulkActionPending}
          onClick={onClear}
          size="sm"
          variant="ghost"
        >
          {t("admin.queueClearSelection")}
        </Button>
      </div>
    </div>
  );
}

function QueuePaginationFooter({
  currentPage,
  onGoToPage,
  onPageSizeChange,
  pageSize,
  t,
  totalItems,
  totalPages,
  visibleEnd,
  visibleStart,
}: QueuePaginationFooterProps) {
  return (
    <TablePaginationFooter
      canGoNext={currentPage < totalPages}
      canGoPrevious={currentPage > 1}
      currentPageNumber={currentPage}
      goToNextPage={() => onGoToPage(Math.min(totalPages, currentPage + 1))}
      goToPage={onGoToPage}
      goToPreviousPage={() => onGoToPage(Math.max(1, currentPage - 1))}
      nextLabel={t("documents.next")}
      onPageSizeChange={onPageSizeChange}
      pageSize={pageSize}
      pageSizeLabel={t("documents.pageSize")}
        pageSizeOptions={TABLE_PAGE_SIZE_OPTIONS}
      previousLabel={t("documents.previous")}
      summary={t("documents.paginationSummary", {
        from: visibleStart,
        to: visibleEnd,
        total: totalItems,
      })}
      totalPages={totalPages}
    />
  );
}
