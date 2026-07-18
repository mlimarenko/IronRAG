import type { TFunction } from 'i18next'

import { TablePaginationFooter } from '@/shared/components/TablePaginationFooter'

import {
  DEFAULT_PAGE_SIZE,
  PAGE_SIZE_OPTIONS,
  type PageSizeOption,
  type UpdateSearchParamState,
} from './documentsPageState'

type DocumentsPaginationFooterProps = {
  canGoNext: boolean
  canGoPrevious: boolean
  currentPageNumber: number
  filteredTotal: number | null
  goToNextPage: () => void
  goToPreviousPage: () => void
  goToPage: (target: number) => void
  itemCount: number
  pageSize: PageSizeOption
  t: TFunction
  totalPages: number | null
  updateSearchParamState: UpdateSearchParamState
  visibleRangeEnd: number
  visibleRangeStart: number
}

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
}: Readonly<DocumentsPaginationFooterProps>) {
  return (
    <TablePaginationFooter
      canGoNext={canGoNext}
      canGoPrevious={canGoPrevious}
      currentPageNumber={currentPageNumber}
      goToNextPage={goToNextPage}
      goToPage={goToPage}
      goToPreviousPage={goToPreviousPage}
      nextLabel={t('documents.next')}
      onPageSizeChange={(value) =>
        updateSearchParamState({
          pageSize: value === DEFAULT_PAGE_SIZE ? null : String(value),
          documentId: null,
        })
      }
      pageSize={pageSize}
      pageSizeLabel={t('documents.pageSize')}
      pageSizeOptions={PAGE_SIZE_OPTIONS}
      previousLabel={t('documents.previous')}
      summary={t('documents.paginationSummary', {
        from: visibleRangeStart,
        to: visibleRangeEnd,
        total: filteredTotal ?? itemCount,
      })}
      totalPages={totalPages}
    />
  )
}
