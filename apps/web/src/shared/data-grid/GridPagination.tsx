import { useTranslation } from 'react-i18next'

import { Button } from '@/shared/ui'

const DEFAULT_PAGE_SIZE_OPTIONS = [25, 50, 100]

interface GridPaginationShared {
  pageSize: number
  onPageSize: (pageSize: number) => void
  pageSizeOptions?: readonly number[]
  pending?: boolean
}

/**
 * Local pagination knows the whole set, so it can state a page count. Remote
 * cursor pagination knows only whether another page exists, so it offers
 * previous/next and a shown range — and states a total only when the server
 * supplied one, never a page number the cursor chain cannot prove.
 */
export type GridPaginationProps =
  | (GridPaginationShared & {
      mode?: 'local'
      page: number
      pageCount: number
      total: number
      onPage: (page: number) => void
    })
  | (GridPaginationShared & {
      mode: 'remote'
      total: number | null
      shown: { from: number; to: number } | null
      canPrevious: boolean
      canNext: boolean
      onPrevious: () => void
      onNext: () => void
    })

function PageSizeSelect({
  pageSize,
  onPageSize,
  pageSizeOptions = DEFAULT_PAGE_SIZE_OPTIONS,
  pending = false,
}: Readonly<GridPaginationShared>) {
  const { t } = useTranslation()
  const sizes = pageSizeOptions.includes(pageSize)
    ? pageSizeOptions
    : [...pageSizeOptions, pageSize].sort((left, right) => left - right)
  return (
    <label className="data-grid__page-size">
      <span>{t('dataGrid.pageSize')}</span>
      <select
        className="input"
        value={pageSize}
        aria-label={t('dataGrid.pageSize')}
        aria-disabled={pending || undefined}
        onChange={(event) => {
          if (!pending) onPageSize(Number(event.target.value))
        }}
      >
        {sizes.map((size) => (
          <option key={size} value={size}>
            {size}
          </option>
        ))}
      </select>
    </label>
  )
}

function LocalPagination(
  props: Readonly<Extract<GridPaginationProps, { mode?: 'local' }> & { mode?: 'local' }>,
) {
  const { t } = useTranslation()
  const { page, pageCount, pageSize, total, onPage, pending = false } = props
  const from = total === 0 ? 0 : (page - 1) * pageSize + 1
  const to = Math.min(page * pageSize, total)
  return (
    <div className="data-grid__pagination" aria-busy={pending}>
      <span>{!pending && t('dataGrid.paginationSummary', { from, to, total })}</span>
      <div className="data-grid__pagination-controls">
        <PageSizeSelect {...props} />
        <Button
          size="sm"
          disabled={page <= 1}
          aria-disabled={pending || undefined}
          onClick={() => {
            if (!pending) onPage(page - 1)
          }}
        >
          {t('dataGrid.previous')}
        </Button>
        <span className="data-grid__page-label">
          {!pending && t('dataGrid.pageOf', { page, pages: pageCount })}
        </span>
        <Button
          size="sm"
          disabled={page >= pageCount}
          aria-disabled={pending || undefined}
          onClick={() => {
            if (!pending) onPage(page + 1)
          }}
        >
          {t('dataGrid.next')}
        </Button>
      </div>
    </div>
  )
}

function RemotePagination(props: Readonly<Extract<GridPaginationProps, { mode: 'remote' }>>) {
  const { t } = useTranslation()
  const { shown, total, canPrevious, canNext, onPrevious, onNext, pending = false } = props
  let summary: string | null = null
  if (shown !== null && total === null) {
    summary = t('dataGrid.paginationSummaryUnknown', { from: shown.from, to: shown.to })
  } else if (shown !== null) {
    summary = t('dataGrid.paginationSummary', { from: shown.from, to: shown.to, total })
  }
  return (
    <div className="data-grid__pagination" aria-busy={pending}>
      <span>{!pending && summary}</span>
      <div className="data-grid__pagination-controls">
        <PageSizeSelect {...props} />
        <Button
          size="sm"
          disabled={!canPrevious}
          aria-disabled={pending || undefined}
          onClick={() => {
            if (!pending) onPrevious()
          }}
        >
          {t('dataGrid.previous')}
        </Button>
        <Button
          size="sm"
          disabled={!canNext}
          aria-disabled={pending || undefined}
          onClick={() => {
            if (!pending) onNext()
          }}
        >
          {t('dataGrid.next')}
        </Button>
      </div>
    </div>
  )
}

/** Footer of every migrated table: what is in view, and how to move. */
export function GridPagination(props: Readonly<GridPaginationProps>) {
  const options = props.pageSizeOptions ?? DEFAULT_PAGE_SIZE_OPTIONS
  const shared: GridPaginationShared = {
    pageSize: props.pageSize,
    onPageSize: props.onPageSize,
    pageSizeOptions: options,
    pending: props.pending ?? false,
  }
  if (props.mode === 'remote') return <RemotePagination {...props} {...shared} />
  return <LocalPagination {...props} {...shared} />
}
