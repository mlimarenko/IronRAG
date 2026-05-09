import type { TFunction } from "i18next";

import { Button } from "@/shared/components/ui/button";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/shared/components/ui/select";

import {
  DEFAULT_PAGE_SIZE,
  PAGE_SIZE_OPTIONS,
  type PageSizeOption,
  type UpdateSearchParamState,
} from "./documentsPageState";

function getPageItems(current: number, total: number): Array<number | "ellipsis"> {
  if (total <= 7) {
    return Array.from({ length: total }, (_, index) => index + 1);
  }
  const items: Array<number | "ellipsis"> = [1];
  const start = Math.max(2, current - 1);
  const end = Math.min(total - 1, current + 1);
  if (start > 2) items.push("ellipsis");
  for (let page = start; page <= end; page += 1) items.push(page);
  if (end < total - 1) items.push("ellipsis");
  items.push(total);
  return items;
}

type DocumentsPaginationFooterProps = {
  canGoNext: boolean;
  canGoPrevious: boolean;
  currentPageNumber: number;
  filteredTotal: number | null;
  goToNextPage: () => void;
  goToPreviousPage: () => void;
  goToPage: (target: number) => void;
  itemCount: number;
  pageSize: PageSizeOption;
  t: TFunction;
  totalPages: number | null;
  updateSearchParamState: UpdateSearchParamState;
  visibleRangeEnd: number;
  visibleRangeStart: number;
};

export function DocumentsPaginationFooter({
  canGoNext,
  canGoPrevious,
  currentPageNumber,
  filteredTotal,
  goToNextPage,
  goToPreviousPage,
  goToPage,
  itemCount,
  pageSize,
  t,
  totalPages,
  updateSearchParamState,
  visibleRangeEnd,
  visibleRangeStart,
}: DocumentsPaginationFooterProps) {
  const effectiveTotal = totalPages ?? currentPageNumber;
  const reachableLimit = currentPageNumber + (canGoNext ? 1 : 0);
  return (
    <div className="shrink-0 border-t bg-background/95 px-4 py-3 shadow-[0_-8px_24px_hsl(var(--background)/0.9)] backdrop-blur supports-[backdrop-filter]:bg-background/85">
      <div className="flex flex-wrap items-center gap-3">
        <span className="text-xs font-medium text-muted-foreground tabular-nums">
          {t("documents.paginationSummary", {
            from: visibleRangeStart,
            to: visibleRangeEnd,
            total: filteredTotal ?? itemCount,
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
                pageSize: value === String(DEFAULT_PAGE_SIZE) ? null : value,
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
        <div className="flex items-center gap-1">
          <Button
            variant="outline"
            size="sm"
            className="h-8 text-xs"
            disabled={!canGoPrevious}
            onClick={goToPreviousPage}
          >
            {t("documents.previous")}
          </Button>
          {getPageItems(currentPageNumber, effectiveTotal).map((item, index) =>
            item === "ellipsis" ? (
              <span
                key={`ellipsis-${index}`}
                className="px-1.5 text-xs text-muted-foreground"
              >
                …
              </span>
            ) : (
              <Button
                key={item}
                variant={item === currentPageNumber ? "default" : "outline"}
                size="sm"
                className="h-8 min-w-8 px-2 text-xs tabular-nums"
                aria-current={item === currentPageNumber ? "page" : undefined}
                disabled={item > reachableLimit}
                onClick={() => goToPage(item)}
              >
                {item}
              </Button>
            ),
          )}
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
  );
}
