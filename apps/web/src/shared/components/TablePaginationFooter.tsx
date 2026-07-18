import type { ReactNode } from 'react'

import { Button } from '@/shared/components/ui/button'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/shared/components/ui/select'

function getPageItems(
  current: number,
  total: number,
): Array<number | 'ellipsis-before' | 'ellipsis-after'> {
  if (total <= 7) {
    return Array.from({ length: total }, (_, index) => index + 1)
  }
  const items: Array<number | 'ellipsis-before' | 'ellipsis-after'> = [1]
  const start = Math.max(2, current - 1)
  const end = Math.min(total - 1, current + 1)
  if (start > 2) items.push('ellipsis-before')
  for (let page = start; page <= end; page += 1) items.push(page)
  if (end < total - 1) items.push('ellipsis-after')
  items.push(total)
  return items
}

type TablePaginationFooterProps<TPageSize extends number> = {
  canGoNext: boolean
  canGoPrevious: boolean
  currentPageNumber: number
  goToNextPage: () => void
  goToPreviousPage: () => void
  goToPage: (target: number) => void
  pageSize: TPageSize
  pageSizeLabel: string
  pageSizeOptions: readonly TPageSize[]
  previousLabel: string
  nextLabel: string
  onPageSizeChange: (pageSize: TPageSize) => void
  summary: ReactNode
  totalPages: number | null
}

export function TablePaginationFooter<TPageSize extends number>({
  canGoNext,
  canGoPrevious,
  currentPageNumber,
  goToNextPage,
  goToPreviousPage,
  goToPage,
  nextLabel,
  onPageSizeChange,
  pageSize,
  pageSizeLabel,
  pageSizeOptions,
  previousLabel,
  summary,
  totalPages,
}: Readonly<TablePaginationFooterProps<TPageSize>>) {
  const effectiveTotal = totalPages ?? currentPageNumber
  const reachableLimit = currentPageNumber + (canGoNext ? 1 : 0)

  return (
    <div className="shrink-0 border-t bg-background/95 px-4 py-3 shadow-[0_-8px_24px_hsl(var(--background)/0.9)] backdrop-blur supports-[backdrop-filter]:bg-background/85">
      <div className="flex flex-wrap items-center gap-3">
        <span className="text-xs font-medium text-muted-foreground tabular-nums">{summary}</span>
        <div className="flex items-center gap-2 md:ml-auto">
          <span className="text-xs text-muted-foreground">{pageSizeLabel}</span>
          <Select
            value={String(pageSize)}
            onValueChange={(value) => onPageSizeChange(Number(value) as TPageSize)}
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
        <div className="flex items-center gap-1">
          <Button
            variant="outline"
            size="sm"
            className="h-8 text-xs"
            disabled={!canGoPrevious}
            onClick={goToPreviousPage}
          >
            {previousLabel}
          </Button>
          {getPageItems(currentPageNumber, effectiveTotal).map((item) =>
            typeof item === 'string' ? (
              <span key={item} className="px-1.5 text-xs text-muted-foreground">
                …
              </span>
            ) : (
              <Button
                key={item}
                variant={item === currentPageNumber ? 'default' : 'outline'}
                size="sm"
                className="h-8 min-w-8 px-2 text-xs tabular-nums"
                aria-current={item === currentPageNumber ? 'page' : undefined}
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
            {nextLabel}
          </Button>
        </div>
      </div>
    </div>
  )
}
