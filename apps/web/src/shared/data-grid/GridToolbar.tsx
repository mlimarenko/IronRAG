import { Search, X } from 'lucide-react'
import type { ReactNode } from 'react'
import { useTranslation } from 'react-i18next'

import { Badge, Button } from '@/shared/ui'
import { gridFilterChips } from './model'
import type { GridColumn, GridFacets, GridState } from './types'

export interface GridToolbarProps<Row> {
  columns: GridColumn<Row>[]
  state: GridState
  facets: GridFacets
  total: number | null
  pending?: boolean
  onSearch: (value: string) => void
  onRemoveFilter: (columnId: string, key: string) => void
  onClearFilters: () => void
  searchLabel?: string
  searchPlaceholder?: string
  extras?: ReactNode
}

/**
 * The single toolbar every migrated table shares: exactly one whole-table
 * search, the active filters as removable chips, a clear-all, and — only when
 * the full filtered count is known and settled — a live count of that set, not
 * of the page in view. A pending request and an unknown server total both
 * suppress the number rather than print a fabricated zero.
 */
export function GridToolbar<Row>({
  columns,
  state,
  facets,
  total,
  pending = false,
  onSearch,
  onRemoveFilter,
  onClearFilters,
  searchLabel,
  searchPlaceholder,
  extras,
}: Readonly<GridToolbarProps<Row>>) {
  const { t } = useTranslation()
  const chips = gridFilterChips(columns, state, facets)
  const label = searchLabel ?? t('common.search')
  const showCount = !pending && total !== null

  return (
    <div className="toolbar data-grid__toolbar">
      <div className="search-box">
        <Search size={16} aria-hidden="true" />
        <input
          className="input"
          type="search"
          value={state.search}
          aria-label={label}
          placeholder={searchPlaceholder ?? t('common.search')}
          onChange={(event) => onSearch(event.target.value)}
        />
      </div>
      {extras}
      {showCount && (
        <span className="data-grid__count" aria-live="polite">
          {t('dataGrid.resultsCount', { count: total })}
        </span>
      )}
      {chips.length > 0 && (
        <fieldset className="data-grid__chips" aria-label={t('dataGrid.activeFilters')}>
          {chips.map((chip) => {
            const valueLabel = chip.value === null ? t('dataGrid.nullValue') : chip.label
            return (
              <Badge key={`${chip.columnId}:${chip.key}`} tone="neutral">
                <button
                  type="button"
                  className="data-grid__chip"
                  aria-label={t('dataGrid.removeFilter', {
                    column: chip.columnLabel,
                    value: valueLabel,
                  })}
                  onClick={() => onRemoveFilter(chip.columnId, chip.key)}
                >
                  <span className="data-grid__chip-label">
                    {chip.columnLabel}: {valueLabel}
                  </span>
                  <X size={12} aria-hidden="true" />
                </button>
              </Badge>
            )
          })}
          <Button size="sm" variant="ghost" onClick={onClearFilters}>
            {t('dataGrid.clearFilters')}
          </Button>
        </fieldset>
      )}
    </div>
  )
}
