import { useDeferredValue, useEffect, useMemo, useState } from "react";
import type { TFunction } from "i18next";
import {
  Copy,
  ExternalLink,
  Globe,
  Loader2,
  RotateCw,
  Search,
  SquareX,
} from "lucide-react";
import { toast } from "sonner";

import {
  documentsApi,
  type WebIngestRunListItem,
  type WebIngestRunPageItem,
} from "@/api";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { ScrollArea } from "@/components/ui/scroll-area";
import { cn } from "@/lib/utils";

const TERMINAL_RUN_STATES = new Set([
  "completed",
  "completed_partial",
  "failed",
  "canceled",
]);
const PAGE_WINDOW_SIZE = 200;
const RUN_COUNT_ORDER = [
  "processed",
  "processing",
  "queued",
  "failed",
  "excluded",
  "duplicates",
  "blocked",
  "canceled",
  "eligible",
  "discovered",
] as const;
const PAGE_STATE_ORDER = [
  "processed",
  "failed",
  "excluded",
  "duplicates",
  "blocked",
  "queued",
  "processing",
  "eligible",
  "discovered",
  "canceled",
] as const;

function runStatusClass(state: string): string {
  switch (state) {
    case "completed":
      return "status-ready";
    case "completed_partial":
      return "status-warning";
    case "failed":
      return "status-failed";
    case "canceled":
      return "status-stalled";
    default:
      return "status-processing";
  }
}

function pageStateDotClass(state: string | undefined): string {
  switch (state) {
    case "processed":
      return "bg-green-500";
    case "failed":
      return "bg-red-500";
    case "excluded":
      return "bg-yellow-500";
    case "duplicates":
      return "bg-sky-500";
    case "blocked":
      return "bg-violet-500";
    case "queued":
      return "bg-slate-400";
    case "processing":
      return "bg-amber-500";
    case "canceled":
      return "bg-slate-500";
    default:
      return "bg-gray-400";
  }
}

function humanizeRunMode(mode: string, t: TFunction): string {
  if (mode === "single_page") return t("documents.singlePage");
  if (mode === "recursive_crawl") return t("documents.recursiveCrawl");
  return mode;
}

function humanizeRunState(state: string, t: TFunction): string {
  const key = `dashboard.runStateLabels.${state}`;
  const translated = t(key);
  return translated === key ? state.replaceAll("_", " ") : translated;
}

function humanizePageState(state: string, t: TFunction): string {
  const key = `documents.pageStateLabels.${state}`;
  const translated = t(key);
  return translated === key ? state.replaceAll("_", " ") : translated;
}

function humanizeUrlFilterMode(mode: string | undefined, t: TFunction): string {
  if (mode === "allowlist") return t("documents.urlFilterModeAllowlist");
  return t("documents.urlFilterModeBlocklist");
}

function pagePrimaryUrl(page: WebIngestRunPageItem): string {
  return (
    page.finalUrl ??
    page.canonicalUrl ??
    page.normalizedUrl ??
    page.discoveredUrl ??
    ""
  );
}

function sortStates(states: string[]): string[] {
  return [...states].sort((a, b) => {
    const aIndex = PAGE_STATE_ORDER.indexOf(
      a as (typeof PAGE_STATE_ORDER)[number],
    );
    const bIndex = PAGE_STATE_ORDER.indexOf(
      b as (typeof PAGE_STATE_ORDER)[number],
    );
    if (aIndex === -1 && bIndex === -1) return a.localeCompare(b);
    if (aIndex === -1) return 1;
    if (bIndex === -1) return -1;
    return aIndex - bIndex;
  });
}

type WebRunsPanelProps = {
  t: TFunction;
  webRuns: WebIngestRunListItem[];
  isRefreshingRuns: boolean;
  onReuseRun: (run: WebIngestRunListItem) => void;
  onRefreshRuns: () => void;
  onCancelRun: (runId: string) => void;
};

export function WebRunsPanel({
  t,
  webRuns,
  isRefreshingRuns,
  onReuseRun,
  onRefreshRuns,
  onCancelRun,
}: WebRunsPanelProps) {
  const [expandedRunId, setExpandedRunId] = useState<string | null>(null);
  const [runPages, setRunPages] = useState<WebIngestRunPageItem[]>([]);
  const [runPagesLoading, setRunPagesLoading] = useState(false);
  const [runPagesError, setRunPagesError] = useState<string | null>(null);
  const [pageStateFilter, setPageStateFilter] = useState<string>("all");
  const [pageSearch, setPageSearch] = useState("");
  const [pageWindowIndex, setPageWindowIndex] = useState(0);
  const [cancelingRunId, setCancelingRunId] = useState<string | null>(null);

  const deferredPageSearch = useDeferredValue(pageSearch.trim().toLowerCase());
  const activeRuns = webRuns.filter(
    (run) => !TERMINAL_RUN_STATES.has(run.runState?.toLowerCase() ?? ""),
  );
  const expandedRun =
    webRuns.find((run) => run.runId === expandedRunId) ?? null;

  const loadRunPages = async (runId: string) => {
    setRunPagesLoading(true);
    setRunPagesError(null);
    try {
      setRunPages(await documentsApi.listWebRunPages(runId));
    } catch {
      setRunPages([]);
      setRunPagesError(t("documents.webRunPagesFailed"));
    } finally {
      setRunPagesLoading(false);
    }
  };

  const handleToggleRun = async (runId: string) => {
    if (expandedRunId === runId) {
      setExpandedRunId(null);
      setRunPages([]);
      setRunPagesError(null);
      return;
    }
    setExpandedRunId(runId);
    void loadRunPages(runId);
  };

  useEffect(() => {
    setPageStateFilter("all");
    setPageSearch("");
    setPageWindowIndex(0);
  }, [expandedRunId]);

  const pageStateCounts = useMemo(() => {
    const counts = new Map<string, number>();
    for (const page of runPages) {
      const state = page.candidateState ?? "unknown";
      counts.set(state, (counts.get(state) ?? 0) + 1);
    }
    return counts;
  }, [runPages]);

  const availablePageStates = useMemo(
    () =>
      sortStates(
        [...pageStateCounts.keys()].filter((state) => state !== "unknown"),
      ),
    [pageStateCounts],
  );

  const filteredRunPages = useMemo(() => {
    return runPages.filter((page) => {
      if (
        pageStateFilter !== "all" &&
        (page.candidateState ?? "unknown") !== pageStateFilter
      ) {
        return false;
      }
      if (!deferredPageSearch) {
        return true;
      }
      const haystack = [
        pagePrimaryUrl(page),
        page.discoveredUrl,
        page.classificationReason,
        page.classificationDetail,
        page.contentType,
      ]
        .filter(Boolean)
        .join(" ")
        .toLowerCase();
      return haystack.includes(deferredPageSearch);
    });
  }, [deferredPageSearch, pageStateFilter, runPages]);

  const totalPageWindows = Math.max(
    1,
    Math.ceil(filteredRunPages.length / PAGE_WINDOW_SIZE),
  );

  useEffect(() => {
    if (pageWindowIndex > totalPageWindows - 1) {
      setPageWindowIndex(Math.max(0, totalPageWindows - 1));
    }
  }, [pageWindowIndex, totalPageWindows]);

  const visiblePages = useMemo(() => {
    const start = pageWindowIndex * PAGE_WINDOW_SIZE;
    return filteredRunPages.slice(start, start + PAGE_WINDOW_SIZE);
  }, [filteredRunPages, pageWindowIndex]);

  const visibleRangeStart =
    filteredRunPages.length === 0 ? 0 : pageWindowIndex * PAGE_WINDOW_SIZE + 1;
  const visibleRangeEnd =
    filteredRunPages.length === 0
      ? 0
      : Math.min(
          filteredRunPages.length,
          (pageWindowIndex + 1) * PAGE_WINDOW_SIZE,
        );

  const handleOpenUrl = (url: string) => {
    if (!url) return;
    window.open(url, "_blank", "noopener,noreferrer");
  };

  const handleCopyUrl = async (url: string) => {
    if (!url) return;
    try {
      await navigator.clipboard.writeText(url);
      toast.success(t("documents.urlCopied"));
    } catch {
      toast.error(t("documents.urlCopyFailed"));
    }
  };

  const handleCancelRun = async (runId: string) => {
    setCancelingRunId(runId);
    try {
      await onCancelRun(runId);
    } finally {
      setCancelingRunId(null);
    }
  };

  const runSummaryItems = useMemo(() => {
    if (!expandedRun?.counts) return [];
    return RUN_COUNT_ORDER.map((key) => ({
      key,
      value: expandedRun.counts?.[key],
    })).filter((item) => (item.value ?? 0) > 0);
  }, [expandedRun]);

  if (webRuns.length === 0) {
    return (
      <div className="empty-state py-20">
        <div className="mb-4 flex h-14 w-14 items-center justify-center rounded-2xl bg-muted">
          <Globe className="h-7 w-7 text-muted-foreground" />
        </div>
        <h2 className="text-base font-bold tracking-tight">
          {t("documents.webIngestRuns")}
        </h2>
        <p className="mt-2 text-sm text-muted-foreground">
          {t("documents.noDocsDesc")}
        </p>
      </div>
    );
  }

  return (
    <div className="flex h-full min-h-0 flex-col">
      {activeRuns.length > 0 && (
        <div className="mx-4 mt-4 flex flex-wrap items-center gap-2">
          <div className="flex items-center gap-2 rounded-xl border bg-card px-3 py-2 text-xs shadow-soft">
            <Loader2 className="h-3 w-3 animate-spin text-primary" />
            <span className="font-semibold">
              {t("documents.webRunActiveSummary", { count: activeRuns.length })}
            </span>
          </div>
        </div>
      )}

      {/* Card + single ScrollArea is the whole scroll contract. The
          single ScrollArea wraps the whole list — nesting a second
          ScrollArea around the page-list traps the wheel event on the
          inner one and makes the outer run list look "frozen" once a
          single run is expanded. Refresh button and run-count header
          live in DocumentsPageHeader and are not duplicated here. */}
      <div className="m-4 flex min-h-0 flex-1 flex-col overflow-hidden rounded-xl border bg-card">
        <ScrollArea className="min-h-0 flex-1">
          <div className="divide-y">
            {webRuns.map((run) => {
              const isExpanded = expandedRunId === run.runId;
              const isCancelable = !TERMINAL_RUN_STATES.has(
                run.runState?.toLowerCase() ?? "",
              );
              return (
                <div key={run.runId}>
                  <div
                    className={cn(
                      "flex items-start gap-3 px-4 py-3",
                      isExpanded && "bg-accent/20",
                    )}
                  >
                    <button
                      type="button"
                      className="min-w-0 flex-1 text-left"
                      onClick={() => void handleToggleRun(run.runId)}
                    >
                      <div className="flex flex-wrap items-center gap-2">
                        <span
                          className={`status-badge ${runStatusClass(run.runState)}`}
                        >
                          {humanizeRunState(run.runState, t)}
                        </span>
                        <span
                          className="truncate text-sm font-semibold"
                          title={run.seedUrl}
                        >
                          {run.seedUrl}
                        </span>
                      </div>
                      <div className="mt-1.5 flex flex-wrap items-center gap-x-3 gap-y-1 text-[11px] text-muted-foreground">
                        <span>{humanizeRunMode(run.mode, t)}</span>
                        {run.mode === "recursive_crawl" && (
                          <span>
                            {t("documents.maxDepth")}: {run.maxDepth} ·{" "}
                            {t("documents.maxPages")}: {run.maxPages}
                          </span>
                        )}
                        <span>
                          {humanizeUrlFilterMode(run.urlFilterMode, t)} ·{" "}
                          {t("documents.urlFilterPatterns")}:{" "}
                          {(run.urlPatterns?.length ?? 0).toLocaleString()}
                        </span>
                        <span>
                          {(run.counts?.processed ?? 0).toLocaleString()} /{" "}
                          {(run.counts?.discovered ?? 0).toLocaleString()}{" "}
                          {t("documents.pages")}
                        </span>
                        {(run.counts?.failed ?? 0) > 0 && (
                          <span>
                            {humanizePageState("failed", t)}:{" "}
                            {(run.counts?.failed ?? 0).toLocaleString()}
                          </span>
                        )}
                        {(run.counts?.excluded ?? 0) > 0 && (
                          <span>
                            {humanizePageState("excluded", t)}:{" "}
                            {(run.counts?.excluded ?? 0).toLocaleString()}
                          </span>
                        )}
                      </div>
                    </button>

                    <div className="flex shrink-0 items-center gap-1">
                      <Button
                        variant="ghost"
                        size="icon"
                        className="h-8 w-8"
                        title={t("documents.openRunUrl")}
                        aria-label={t("documents.openRunUrl")}
                        onClick={() => handleOpenUrl(run.seedUrl)}
                      >
                        <ExternalLink className="h-3.5 w-3.5" />
                      </Button>
                      <Button
                        variant="ghost"
                        size="icon"
                        className="h-8 w-8"
                        title={t("documents.copyUrl")}
                        aria-label={t("documents.copyUrl")}
                        onClick={() => void handleCopyUrl(run.seedUrl)}
                      >
                        <Copy className="h-3.5 w-3.5" />
                      </Button>
                      <Button
                        variant="ghost"
                        size="icon"
                        className="h-8 w-8"
                        title={t("documents.reuseRunSettings")}
                        aria-label={t("documents.reuseRunSettings")}
                        onClick={() => onReuseRun(run)}
                      >
                        <RotateCw className="h-3.5 w-3.5" />
                      </Button>
                      {isCancelable && (
                        <Button
                          variant="ghost"
                          size="icon"
                          className="h-8 w-8 text-destructive hover:text-destructive"
                          disabled={cancelingRunId === run.runId}
                          title={t("documents.cancelRun")}
                          aria-label={t("documents.cancelRun")}
                          onClick={() => void handleCancelRun(run.runId)}
                        >
                          {cancelingRunId === run.runId ? (
                            <Loader2 className="h-3.5 w-3.5 animate-spin" />
                          ) : (
                            <SquareX className="h-3.5 w-3.5" />
                          )}
                        </Button>
                      )}
                    </div>
                  </div>

                  {isExpanded && (
                    <div className="border-t bg-surface-sunken/35">
                      <div className="border-b px-4 py-3">
                        <div className="flex flex-wrap items-center gap-2">
                          {runSummaryItems.map((item) => (
                            <span
                              key={item.key}
                              className="rounded-full border bg-background px-2.5 py-1 text-[11px] text-muted-foreground"
                            >
                              {humanizePageState(item.key, t)}:{" "}
                              <span className="font-semibold text-foreground">
                                {(item.value ?? 0).toLocaleString()}
                              </span>
                            </span>
                          ))}
                        </div>

                        <div className="mt-3 flex flex-col gap-3 xl:flex-row xl:items-center">
                          <div className="relative w-full xl:max-w-sm">
                            <Search className="absolute left-3 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
                            <Input
                              value={pageSearch}
                              onChange={(event) =>
                                setPageSearch(event.target.value)
                              }
                              className="h-8 pl-9 text-xs"
                              placeholder={t("documents.pageSearchPlaceholder")}
                            />
                          </div>
                          <div className="flex min-w-0 flex-wrap gap-1">
                            {/* Filter chips render as labels only —
                                counts already live in the summary strip
                                above (processed / excluded / matched /
                                found), so duplicating them on the chips
                                pushed the filter row across multiple
                                lines on narrow viewports. */}
                            <button
                              type="button"
                              className={cn(
                                "cursor-pointer rounded-lg px-2.5 py-1 text-[11px] transition-colors",
                                pageStateFilter === "all"
                                  ? "bg-background font-semibold text-foreground shadow-soft"
                                  : "text-muted-foreground hover:bg-background/70 hover:text-foreground",
                              )}
                              onClick={() => setPageStateFilter("all")}
                            >
                              {t("documents.all")}
                            </button>
                            {availablePageStates.map((state) => (
                              <button
                                key={state}
                                type="button"
                                className={cn(
                                  "cursor-pointer rounded-lg px-2.5 py-1 text-[11px] transition-colors",
                                  pageStateFilter === state
                                    ? "bg-background font-semibold text-foreground shadow-soft"
                                    : "text-muted-foreground hover:bg-background/70 hover:text-foreground",
                                )}
                                onClick={() => setPageStateFilter(state)}
                              >
                                {humanizePageState(state, t)}
                              </button>
                            ))}
                          </div>
                          <div className="flex items-center gap-2 xl:ml-auto">
                            <span className="text-[11px] text-muted-foreground">
                              {t("documents.pageWindowSummary", {
                                from: visibleRangeStart,
                                to: visibleRangeEnd,
                                total: filteredRunPages.length,
                              })}
                            </span>
                            <Button
                              variant="outline"
                              size="sm"
                              className="h-8 text-xs"
                              disabled={runPagesLoading}
                              onClick={() => void loadRunPages(run.runId)}
                            >
                              {runPagesLoading ? (
                                <Loader2 className="mr-1.5 h-3.5 w-3.5 animate-spin" />
                              ) : (
                                <RotateCw className="mr-1.5 h-3.5 w-3.5" />
                              )}
                              {t("documents.refreshRunPages")}
                            </Button>
                          </div>
                        </div>
                      </div>

                      {runPagesLoading ? (
                        <div className="flex items-center gap-2 px-4 py-6 text-sm text-muted-foreground">
                          <Loader2 className="h-4 w-4 animate-spin text-primary" />
                          {t("documents.loadingPages")}
                        </div>
                      ) : runPagesError ? (
                        <div className="px-4 py-4">
                          <div className="inline-error">{runPagesError}</div>
                        </div>
                      ) : filteredRunPages.length === 0 ? (
                        <div className="px-4 py-8 text-center">
                          <div className="text-sm font-semibold">
                            {t("documents.noMatchingPages")}
                          </div>
                          <div className="mt-1 text-xs text-muted-foreground">
                            {runPages.length === 0
                              ? t("documents.noMatchingPagesDesc")
                              : t("documents.noMatchingPagesFilteredDesc")}
                          </div>
                        </div>
                      ) : (
                        <>
                          <div className="space-y-2 px-4 py-3">
                            {visiblePages.map((page) => {
                              const url = pagePrimaryUrl(page);
                              return (
                                <div
                                  key={
                                    page.candidateId ?? `${page.runId}-${url}`
                                  }
                                  className="flex items-start gap-3 rounded-xl border bg-background/80 px-3 py-2.5"
                                >
                                  <span
                                    className={cn(
                                      "mt-1.5 h-2 w-2 shrink-0 rounded-full",
                                      pageStateDotClass(page.candidateState),
                                    )}
                                  />
                                  <div className="min-w-0 flex-1">
                                    <div
                                      className="truncate text-xs font-medium"
                                      title={url}
                                    >
                                      {url || "?"}
                                    </div>
                                    <div className="mt-1 flex flex-wrap items-center gap-x-3 gap-y-1 text-[10px] text-muted-foreground">
                                      <span>
                                        {humanizePageState(
                                          page.candidateState ?? "unknown",
                                          t,
                                        )}
                                      </span>
                                      {page.depth != null && (
                                        <span>
                                          {t("documents.maxDepth")}:{" "}
                                          {page.depth}
                                        </span>
                                      )}
                                      {page.httpStatus != null && (
                                        <span>HTTP {page.httpStatus}</span>
                                      )}
                                      {page.contentType && (
                                        <span>{page.contentType}</span>
                                      )}
                                      {page.classificationReason && (
                                        <span title={page.classificationReason}>
                                          {page.classificationReason}
                                        </span>
                                      )}
                                      {page.classificationDetail && (
                                        <span
                                          className="max-w-full truncate"
                                          title={page.classificationDetail}
                                        >
                                          {page.classificationDetail}
                                        </span>
                                      )}
                                    </div>
                                  </div>
                                  <div className="flex shrink-0 items-center gap-1">
                                    <Button
                                      variant="ghost"
                                      size="icon"
                                      className="h-7 w-7"
                                      title={t("documents.openPage")}
                                      aria-label={t("documents.openPage")}
                                      onClick={() => handleOpenUrl(url)}
                                    >
                                      <ExternalLink className="h-3.5 w-3.5" />
                                    </Button>
                                    <Button
                                      variant="ghost"
                                      size="icon"
                                      className="h-7 w-7"
                                      title={t("documents.copyUrl")}
                                      aria-label={t("documents.copyUrl")}
                                      onClick={() => void handleCopyUrl(url)}
                                    >
                                      <Copy className="h-3.5 w-3.5" />
                                    </Button>
                                  </div>
                                </div>
                              );
                            })}
                          </div>

                          {filteredRunPages.length > PAGE_WINDOW_SIZE && (
                            <div className="flex items-center justify-between border-t px-4 py-3">
                              <span className="text-[11px] text-muted-foreground">
                                {t("documents.pageLabel", {
                                  page: pageWindowIndex + 1,
                                  total: totalPageWindows,
                                })}
                              </span>
                              <div className="flex items-center gap-2">
                                <Button
                                  variant="outline"
                                  size="sm"
                                  className="h-8 text-xs"
                                  disabled={pageWindowIndex === 0}
                                  onClick={() =>
                                    setPageWindowIndex((current) =>
                                      Math.max(0, current - 1),
                                    )
                                  }
                                >
                                  {t("documents.previous")}
                                </Button>
                                <Button
                                  variant="outline"
                                  size="sm"
                                  className="h-8 text-xs"
                                  disabled={
                                    pageWindowIndex >= totalPageWindows - 1
                                  }
                                  onClick={() =>
                                    setPageWindowIndex((current) =>
                                      Math.min(
                                        totalPageWindows - 1,
                                        current + 1,
                                      ),
                                    )
                                  }
                                >
                                  {t("documents.next")}
                                </Button>
                              </div>
                            </div>
                          )}
                        </>
                      )}
                    </div>
                  )}
                </div>
              );
            })}
          </div>
        </ScrollArea>
      </div>
    </div>
  );
}
