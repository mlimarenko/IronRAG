import * as DropdownMenu from '@radix-ui/react-dropdown-menu'
import { ArrowDown, ArrowUp, Check, Filter, LoaderCircle, Search, X } from 'lucide-react'
import { useMemo, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'

import { gridFacetValue } from './model'
import type {
  GridColumn,
  GridFacetOption,
  GridFacetPage,
  GridSortDirection,
  GridState,
} from './types'

/** A count below zero means "the server did not tell us"; it is never rendered. */
const UNKNOWN_COUNT = -1

function matchesValueQuery(option: GridFacetOption, needle: string, nullLabel: string): boolean {
  if (!needle) return true
  const haystack = `${option.value === null ? nullLabel : option.label} ${String(option.value)}`
  return haystack.toLowerCase().includes(needle)
}

/**
 * A selected value the current server page did not return stays visible and
 * unchecked-counted: the operator can still see and clear the constraint, and
 * no number is invented for it.
 */
function withSelectedValues(
  options: GridFacetOption[],
  selected: readonly string[],
): GridFacetOption[] {
  const present = new Set(options.map((option) => option.key))
  const missing = selected.filter((key) => !present.has(key))
  if (missing.length === 0) return options
  return [
    ...options,
    ...missing.map((key) => {
      const value = gridFacetValue(key)
      return {
        key,
        value,
        label: value === null ? '' : String(value),
        count: UNKNOWN_COUNT,
      }
    }),
  ]
}

function FacetOptionRow({
  option,
  selected,
  onToggle,
}: Readonly<{
  option: GridFacetOption
  selected: readonly string[]
  onToggle: (key: string) => void
}>) {
  const { t } = useTranslation()
  const label = option.value === null ? t('dataGrid.nullValue') : option.label
  const knownCount = option.count >= 0
  return (
    <DropdownMenu.CheckboxItem
      className="data-grid__menu-item data-grid__menu-item--check"
      aria-label={label}
      {...(knownCount
        ? { 'aria-description': t('dataGrid.resultsCount', { count: option.count }) }
        : {})}
      checked={selected.includes(option.key)}
      // Keeping the menu open is the point: filtering several values is
      // one decision, not several trips through the header.
      onSelect={(event: Event) => event.preventDefault()}
      onCheckedChange={() => onToggle(option.key)}
    >
      <DropdownMenu.ItemIndicator className="data-grid__menu-check">
        <Check size={13} aria-hidden="true" />
      </DropdownMenu.ItemIndicator>
      <span className="data-grid__filter-label" title={option.title}>
        {label}
      </span>
      {knownCount && (
        <span className="data-grid__filter-count" aria-hidden="true">
          {option.count}
        </span>
      )}
    </DropdownMenu.CheckboxItem>
  )
}

function GridFacetFilterList({
  columnId,
  columnLabel,
  options,
  selected,
  serverPage,
  onToggle,
  onClear,
  onSearch,
  onLoadMore,
}: Readonly<{
  columnId: string
  columnLabel: string
  options: GridFacetOption[]
  selected: string[]
  serverPage?: GridFacetPage | undefined
  onToggle: (columnId: string, key: string) => void
  onClear: (columnId: string) => void
  onSearch?: ((columnId: string, query: string) => void) | undefined
  onLoadMore?: ((columnId: string) => void) | undefined
}>) {
  const { t } = useTranslation()
  const [query, setQuery] = useState('')
  const needle = query.trim().toLowerCase()
  const nullLabel = t('dataGrid.nullValue')
  // A server-driven menu shows exactly the values the server returned; its own
  // search is a server facet search, never a client-side narrowing of a
  // partial page presented as the full set.
  const serverDriven = serverPage !== undefined
  const visible = useMemo(() => {
    const base = serverDriven ? (serverPage?.options ?? []) : options
    const withSelected = withSelectedValues(base, selected)
    if (serverDriven) return withSelected
    return withSelected.filter((option) => matchesValueQuery(option, needle, nullLabel))
  }, [options, serverPage, serverDriven, selected, needle, nullLabel])

  const pending = serverPage?.pending ?? false
  const error = serverPage?.error ?? false
  return (
    <div className="data-grid__filter">
      <label className="data-grid__filter-search">
        <Search size={13} aria-hidden="true" />
        <input
          type="search"
          className="input"
          value={query}
          placeholder={t('dataGrid.filterSearch')}
          aria-label={t('dataGrid.filterSearch')}
          onChange={(event) => {
            setQuery(event.target.value)
            onSearch?.(columnId, event.target.value)
          }}
          // A menu answers a typed character with typeahead that would move
          // focus onto a matching item and away from this box. Only printable
          // keys are stopped; Escape still reaches the menu.
          onKeyDown={(event) => {
            if (event.key.length === 1) event.stopPropagation()
          }}
        />
      </label>
      <fieldset
        className="data-grid__filter-list"
        aria-label={t('dataGrid.filterValues', { column: columnLabel })}
      >
        {pending && (
          <p className="data-grid__menu-empty" aria-live="polite">
            <LoaderCircle className="spin" size={13} aria-hidden="true" />
            {t('dataGrid.valuesLoading')}
          </p>
        )}
        {!pending && error && visible.length === 0 && (
          <p className="data-grid__menu-empty">{t('dataGrid.valuesFailed')}</p>
        )}
        {!(error && !pending && visible.length === 0) &&
          visible.map((option) => (
            <FacetOptionRow
              key={option.key}
              option={option}
              selected={selected}
              onToggle={(key) => onToggle(columnId, key)}
            />
          ))}
        {!pending && !error && visible.length === 0 && (
          <p className="data-grid__menu-empty">{t('dataGrid.noValues')}</p>
        )}
      </fieldset>
      {serverPage?.hasMore && (
        <DropdownMenu.Item
          className="data-grid__menu-item"
          disabled={pending}
          // Loading another page of values is part of filtering, not a
          // dismissal: keep the menu open so the OR selection is one action.
          onSelect={(event: Event) => {
            event.preventDefault()
            onLoadMore?.(columnId)
          }}
        >
          {pending ? (
            <LoaderCircle className="spin" size={13} aria-hidden="true" />
          ) : (
            <ArrowDown size={13} aria-hidden="true" />
          )}
          {t('dataGrid.loadMoreValues')}
        </DropdownMenu.Item>
      )}
      <DropdownMenu.Item
        className="data-grid__menu-item"
        disabled={selected.length === 0}
        onSelect={() => onClear(columnId)}
      >
        <X size={13} aria-hidden="true" />
        {t('dataGrid.clearColumn')}
      </DropdownMenu.Item>
    </div>
  )
}

export interface GridColumnMenuProps<Row> {
  column: GridColumn<Row>
  options: GridFacetOption[]
  state: GridState
  serverPage?: GridFacetPage | undefined
  onOpen?: ((columnId: string) => void) | undefined
  onSearch?: ((columnId: string, query: string) => void) | undefined
  onLoadMore?: ((columnId: string) => void) | undefined
  onSetSort: (columnId: string, direction: GridSortDirection) => void
  onClearSort: (columnId: string) => void
  onToggleFilterValue: (columnId: string, key: string) => void
  onClearColumnFilter: (columnId: string) => void
}

/**
 * The header control of one data column: ascending/descending/clear for the
 * ordering and a checkbox list of the values the other filters leave, computed
 * by the caller. It is the only place a column is sorted or filtered.
 */
export function GridColumnMenu<Row>({
  column,
  options,
  state,
  serverPage,
  onOpen,
  onSearch,
  onLoadMore,
  onSetSort,
  onClearSort,
  onToggleFilterValue,
  onClearColumnFilter,
}: Readonly<GridColumnMenuProps<Row>>) {
  const { t } = useTranslation()
  const [open, setOpen] = useState(false)
  const triggerRef = useRef<HTMLButtonElement>(null)
  const direction = state.sort?.columnId === column.id ? state.sort.direction : null
  const selected = state.filters[column.id] ?? []

  return (
    <DropdownMenu.Root
      open={open}
      onOpenChange={(next) => {
        setOpen(next)
        if (next) onOpen?.(column.id)
      }}
    >
      <DropdownMenu.Trigger asChild>
        <button
          ref={triggerRef}
          type="button"
          className="data-grid__head-button"
          aria-label={t('dataGrid.columnMenu', { column: column.label })}
          onKeyDown={(event) => {
            if (event.key === 'Escape') {
              setOpen(false)
              triggerRef.current?.focus()
            }
          }}
        >
          <span className="data-grid__head-label">{column.label}</span>
          {direction === 'asc' && <ArrowUp size={11} aria-hidden="true" />}
          {direction === 'desc' && <ArrowDown size={11} aria-hidden="true" />}
          {selected.length > 0 && (
            <Filter className="data-grid__head-filter" size={11} aria-hidden="true" />
          )}
        </button>
      </DropdownMenu.Trigger>
      <DropdownMenu.Portal>
        <DropdownMenu.Content
          className="data-grid__menu"
          align="start"
          sideOffset={4}
          collisionPadding={8}
          aria-label={t('dataGrid.columnMenu', { column: column.label })}
        >
          <DropdownMenu.Item
            className="data-grid__menu-item"
            onSelect={() => onSetSort(column.id, 'asc')}
          >
            <ArrowUp size={13} aria-hidden="true" />
            {t('dataGrid.sortAscending')}
            {direction === 'asc' && (
              <Check className="data-grid__menu-check" size={13} aria-hidden="true" />
            )}
          </DropdownMenu.Item>
          <DropdownMenu.Item
            className="data-grid__menu-item"
            onSelect={() => onSetSort(column.id, 'desc')}
          >
            <ArrowDown size={13} aria-hidden="true" />
            {t('dataGrid.sortDescending')}
            {direction === 'desc' && (
              <Check className="data-grid__menu-check" size={13} aria-hidden="true" />
            )}
          </DropdownMenu.Item>
          <DropdownMenu.Item
            className="data-grid__menu-item"
            disabled={direction === null}
            onSelect={() => onClearSort(column.id)}
          >
            <X size={13} aria-hidden="true" />
            {t('dataGrid.clearSort')}
          </DropdownMenu.Item>
          <>
            <DropdownMenu.Separator className="data-grid__menu-separator" />
            <GridFacetFilterList
              columnId={column.id}
              columnLabel={column.label}
              options={options}
              selected={selected}
              serverPage={serverPage}
              onToggle={onToggleFilterValue}
              onClear={onClearColumnFilter}
              onSearch={onSearch}
              onLoadMore={onLoadMore}
            />
          </>
        </DropdownMenu.Content>
      </DropdownMenu.Portal>
    </DropdownMenu.Root>
  )
}
