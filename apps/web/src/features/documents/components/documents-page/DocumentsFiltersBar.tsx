import type { TFunction } from "i18next";
import {
  Ban,
  CheckCircle2,
  CheckSquare,
  Clock,
  Columns3,
  Hourglass,
  Search,
  XCircle,
} from "lucide-react";

import { Button } from "@/shared/components/ui/button";
import { Input } from "@/shared/components/ui/input";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/shared/components/ui/tooltip";
import type { DocumentListStatusCounts } from "@/shared/api/generated";

import type {
  DocumentsStatusBucket,
  UpdateSearchParamState,
} from "./documentsPageState";

type DocumentsFiltersBarProps = {
  libraryCost: number;
  onCancelSelection: () => void;
  onStartSelection: () => void;
  onToggleDetailColumns: () => void;
  searchQuery: string;
  selectionMode: boolean;
  showDetailColumns: boolean;
  statusBucket: DocumentsStatusBucket;
  statusCounts: DocumentListStatusCounts | null;
  t: TFunction;
  updateSearchParamState: UpdateSearchParamState;
  workspaceCost: number;
};

export function DocumentsFiltersBar({
  libraryCost,
  onCancelSelection,
  onStartSelection,
  onToggleDetailColumns,
  searchQuery,
  selectionMode,
  showDetailColumns,
  statusBucket,
  statusCounts,
  t,
  updateSearchParamState,
  workspaceCost,
}: DocumentsFiltersBarProps) {
  const showCostSummary = libraryCost > 0 || workspaceCost > 0;
  const buckets = [
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
      icon: <CheckCircle2 className="h-3.5 w-3.5 text-status-ready" />,
    },
    {
      key: "processing" as const,
      label: t("documents.statusProcessing"),
      count: statusCounts?.processing ?? null,
      icon: <Clock className="h-3.5 w-3.5 text-status-processing" />,
    },
    {
      key: "queued" as const,
      label: t("documents.statusQueued"),
      count: statusCounts?.queued ?? null,
      icon: <Hourglass className="h-3.5 w-3.5 text-status-queued" />,
    },
    {
      key: "failed" as const,
      label: t("documents.statusFailed"),
      count: statusCounts?.failed ?? null,
      icon: <XCircle className="h-3.5 w-3.5 text-status-failed" />,
    },
    {
      key: "canceled" as const,
      label: t("documents.statusCanceled"),
      count: statusCounts?.canceled ?? null,
      icon: <Ban className="h-3.5 w-3.5 text-status-stalled" />,
    },
  ];

  return (
    <div className="px-6 py-3 border-b flex flex-wrap items-center gap-3 bg-surface-sunken/50">
      <div className="relative flex-1 min-w-[200px] max-w-md">
        <Search className="absolute left-3 top-1/2 -translate-y-1/2 h-3.5 w-3.5 text-muted-foreground" />
        <Input
          className="h-9 pl-9 text-sm"
          placeholder={t("documents.searchPlaceholder")}
          value={searchQuery}
          onChange={(event) =>
            updateSearchParamState({
              q: event.target.value || null,
              documentId: null,
            })
          }
        />
      </div>
      <div className="flex flex-wrap gap-0.5 p-1 bg-surface-sunken rounded-lg">
        {buckets.map((bucket) => {
          const active = statusBucket === bucket.key;
          return (
            <button
              key={bucket.key}
              type="button"
              className={`px-3 py-1.5 text-xs rounded-md transition-all duration-200 font-medium flex items-center gap-1.5 ${
                active
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
              {active && bucket.count != null && bucket.count > 0 && (
                <span className="tabular-nums text-2xs opacity-70">
                  {bucket.count}
                </span>
              )}
            </button>
          );
        })}
      </div>
      {/* Reserve the cost-summary slot so toggling cost > 0 does not shove the
          action buttons sideways (DOC-11). The slot collapses its content but
          keeps the right-aligned action group anchored via ml-auto. */}
      <div className="ml-auto flex items-center gap-3">
        <Tooltip>
          <TooltipTrigger asChild>
            <Button
              size="sm"
              variant={showDetailColumns ? "default" : "outline"}
              className="h-8 text-xs"
              aria-pressed={showDetailColumns}
              onClick={onToggleDetailColumns}
            >
              <Columns3 className="h-3.5 w-3.5 mr-1.5" />
              {t("documents.detailColumns")}
            </Button>
          </TooltipTrigger>
          <TooltipContent className="max-w-64">
            {t("documents.detailColumnsHint")}
          </TooltipContent>
        </Tooltip>
        <Button
          size="sm"
          variant={selectionMode ? "default" : "outline"}
          className="h-8 text-xs"
          onClick={selectionMode ? onCancelSelection : onStartSelection}
        >
          <CheckSquare className="h-3.5 w-3.5 mr-1.5" />
          {selectionMode ? t("documents.cancelSelection") : t("documents.select")}
        </Button>
        {showCostSummary && (
          <div className="hidden flex-wrap items-center gap-x-3 gap-y-1 text-xs text-muted-foreground lg:flex">
            <span>
              {t("documents.libraryCost")}:{" "}
              <span className="font-bold tabular-nums">
                ${libraryCost.toFixed(3)}
              </span>
            </span>
            <span>
              {t("documents.workspaceCost")}:{" "}
              <span className="font-bold tabular-nums">
                ${workspaceCost.toFixed(3)}
              </span>
            </span>
          </div>
        )}
      </div>
    </div>
  );
}
