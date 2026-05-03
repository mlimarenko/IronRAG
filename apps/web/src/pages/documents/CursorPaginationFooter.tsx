import type { TFunction } from "i18next";

import { Button } from "@/components/ui/button";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";

type CursorPaginationFooterProps<TPageSize extends number> = {
  t: TFunction;
  from: number;
  to: number;
  total: number;
  pageSize: TPageSize;
  pageSizeOptions: readonly TPageSize[];
  onPageSizeChange: (pageSize: TPageSize) => void;
  currentPage: number;
  totalPages: number | null;
  canGoPrevious: boolean;
  canGoNext: boolean;
  onPrevious: () => void;
  onNext: () => void;
};

export function CursorPaginationFooter<TPageSize extends number>({
  t,
  from,
  to,
  total,
  pageSize,
  pageSizeOptions,
  onPageSizeChange,
  currentPage,
  totalPages,
  canGoPrevious,
  canGoNext,
  onPrevious,
  onNext,
}: CursorPaginationFooterProps<TPageSize>) {
  return (
    <div className="shrink-0 border-t bg-background/95 px-4 py-3 shadow-[0_-8px_24px_hsl(var(--background)/0.9)] backdrop-blur supports-[backdrop-filter]:bg-background/85">
      <div className="flex flex-wrap items-center gap-3">
        <span className="text-xs font-medium text-muted-foreground tabular-nums">
          {t("documents.paginationSummary", { from, to, total })}
        </span>

        <div className="flex items-center gap-2 md:ml-auto">
          <span className="text-xs text-muted-foreground">
            {t("documents.pageSize")}
          </span>
          <Select
            value={String(pageSize)}
            onValueChange={(value) => {
              const next = pageSizeOptions.find(
                (option) => String(option) === value,
              );
              if (next != null) onPageSizeChange(next);
            }}
          >
            <SelectTrigger className="h-8 w-[92px] text-xs">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {pageSizeOptions.map((option) => (
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
            onClick={onPrevious}
          >
            {t("documents.previous")}
          </Button>
          <span className="min-w-[112px] text-center text-xs font-medium text-muted-foreground tabular-nums">
            {totalPages != null
              ? t("documents.pageLabel", {
                  page: currentPage,
                  total: totalPages,
                })
              : t("documents.pageLabelSimple", { page: currentPage })}
          </span>
          <Button
            variant="outline"
            size="sm"
            className="h-8 text-xs"
            disabled={!canGoNext}
            onClick={onNext}
          >
            {t("documents.next")}
          </Button>
        </div>
      </div>
    </div>
  );
}
