import { AlertTriangle, Check, Inbox, LoaderCircle, SearchX, type LucideIcon } from 'lucide-react'
import { useEffect, useMemo, useRef, type ReactNode } from 'react'
import { useTranslation } from 'react-i18next'

import { Button } from '@/shared/ui'
import { GridColumnMenu } from './GridColumnMenu'
import { GridPagination } from './GridPagination'
import { GridToolbar } from './GridToolbar'
import {
  gridColumnDisplayText,
  gridPageCount,
  reduceGridState,
  resolveGridData,
  type GridAction,
  type ResolvedGridData,
} from './model'
import type {
  DataGridProps,
  GridColumn,
  GridFacetOption,
  GridFacetPage,
  GridFacets,
  GridSelection,
  GridServerFacets,
  GridServiceColumn,
  GridState,
} from './types'

function cellContent<Row>(column: GridColumn<Row>, row: Row): ReactNode {
  if (column.cell) return column.cell(row)
  return gridColumnDisplayText(column, row)
}

interface FacetQuery {
  options: GridFacetOption[]
  page: ((columnId: string) => GridFacetPage | undefined) | undefined
  onOpen: ((columnId: string) => void) | undefined
  onSearch: ((columnId: string, query: string) => void) | undefined
  onLoadMore: ((columnId: string) => void) | undefined
}

type GridStateKind = 'empty' | 'noMatch' | 'loading' | 'error'

type GridPhase = 'error' | 'pending' | 'ready'

/** What the controlled renderer resolves to: a remote page or the local pipeline. */
type GridResolved<Row> =
  | { kind: 'remote'; rows: Row[]; total: number | null; pageCount: number; facets: GridFacets }
  | ({ kind: 'local' } & ResolvedGridData<Row>)

const GRID_STATE_ICONS: Record<GridStateKind, LucideIcon> = {
  empty: Inbox,
  noMatch: SearchX,
  loading: LoaderCircle,
  error: AlertTriangle,
}

function gridPhase(errorText: string | undefined, pending: boolean): GridPhase {
  if (errorText !== undefined) return 'error'
  if (pending) return 'pending'
  return 'ready'
}

function facetQueryFor(serverFacets: GridServerFacets | undefined): FacetQuery {
  if (!serverFacets) {
    return {
      options: [],
      page: undefined,
      onOpen: undefined,
      onSearch: undefined,
      onLoadMore: undefined,
    }
  }
  return {
    options: [],
    page: serverFacets.page,
    onOpen: serverFacets.onOpen,
    onSearch: serverFacets.onSearch,
    onLoadMore: serverFacets.onLoadMore,
  }
}

function GridHeaderCell<Row>({
  column,
  facets,
  state,
  dispatch,
}: Readonly<{
  column: GridColumn<Row>
  facets: FacetQuery
  state: GridState
  dispatch: (action: GridAction) => void
}>) {
  const sorted = state.sort?.columnId === column.id ? state.sort.direction : null
  const ariaSort = sorted ? ({ asc: 'ascending', desc: 'descending' } as const)[sorted] : undefined
  const serverPage = facets.page?.(column.id)
  const options = serverPage?.options ?? facets.options
  return (
    <th scope="col" aria-sort={ariaSort} data-column={column.id}>
      <GridColumnMenu
        column={column}
        options={options}
        state={state}
        serverPage={serverPage}
        onOpen={facets.onOpen}
        onSearch={facets.onSearch}
        onLoadMore={facets.onLoadMore}
        onSetSort={(columnId, direction) => dispatch({ type: 'sort', columnId, direction })}
        onClearSort={(columnId) => dispatch({ type: 'clearSort', columnId })}
        onToggleFilterValue={(columnId, key) => dispatch({ type: 'toggleFilter', columnId, key })}
        onClearColumnFilter={(columnId) => dispatch({ type: 'clearColumnFilter', columnId })}
      />
    </th>
  )
}

function GridSelectHead<Row>({
  selection,
  rows,
  rowId,
}: Readonly<{
  selection: Extract<GridSelection<Row>, { mode: 'multiple' }>
  rows: Row[]
  rowId: (row: Row) => string
}>) {
  const ref = useRef<HTMLInputElement>(null)
  const selectable = rows.filter((row) => selection.isSelectable?.(row) ?? true)
  const selectedCount = selectable.filter((row) => selection.selectedIds.has(rowId(row))).length
  const allSelected = selectable.length > 0 && selectedCount === selectable.length
  const someSelected = selectedCount > 0 && !allSelected
  useEffect(() => {
    if (ref.current) ref.current.indeterminate = someSelected
  }, [someSelected])
  return (
    <th scope="col" className="data-grid__select-head">
      <input
        ref={ref}
        type="checkbox"
        checked={allSelected}
        disabled={selectable.length === 0}
        aria-label={selection.headerLabel}
        onChange={(event) => selection.onToggleAll(event.target.checked, rows)}
      />
    </th>
  )
}

function GridSelectCell<Row>({
  row,
  rowId,
  selection,
}: Readonly<{ row: Row; rowId: string; selection: GridSelection<Row> }>) {
  const selectable = selection.isSelectable?.(row) ?? true
  if (selection.mode === 'multiple') {
    return (
      <td className="data-grid__select-cell">
        <input
          type="checkbox"
          checked={selection.selectedIds.has(rowId)}
          disabled={!selectable}
          aria-label={selection.rowLabel(row)}
          onClick={(event) => event.stopPropagation()}
          onChange={() => {
            if (selectable) selection.onToggle(row)
          }}
        />
      </td>
    )
  }
  const active = selection.activeRowId === rowId
  return (
    <td className="data-grid__select-cell">
      <button
        type="button"
        className="data-grid__select"
        aria-pressed={active}
        aria-disabled={!selectable}
        aria-label={selection.rowLabel(row)}
        onClick={(event) => {
          event.stopPropagation()
          if (selectable) selection.onActivate(row)
        }}
      >
        {active && <Check size={14} aria-hidden="true" />}
      </button>
    </td>
  )
}

function rowIsSelected<Row>(selection: GridSelection<Row>, rowId: string): boolean {
  return selection.mode === 'multiple'
    ? selection.selectedIds.has(rowId)
    : selection.activeRowId === rowId
}

function activateRow<Row>(selection: GridSelection<Row>, row: Row): void {
  if (selection.mode === 'multiple') selection.onToggle(row)
  else selection.onActivate(row)
}

function GridBodyRow<Row>({
  row,
  rowId,
  columns,
  serviceColumns,
  selection,
}: Readonly<{
  row: Row
  rowId: string
  columns: GridColumn<Row>[]
  serviceColumns: GridServiceColumn<Row>[]
  selection?: GridSelection<Row> | undefined
}>) {
  const selectable = selection ? (selection.isSelectable?.(row) ?? true) : false
  const active = selection ? rowIsSelected(selection, rowId) : false
  const rowClass = [
    'data-grid__row',
    active && 'is-selected',
    selection && !selectable && 'is-blocked',
  ]
    .filter(Boolean)
    .join(' ')
  return (
    <tr
      className={rowClass}
      data-row-id={rowId}
      aria-selected={selection ? active : undefined}
      onClick={
        selection && selectable
          ? () => {
              activateRow(selection, row)
            }
          : undefined
      }
    >
      {selection && <GridSelectCell row={row} rowId={rowId} selection={selection} />}
      {columns.map((column) => (
        <td key={column.id} className={column.align === 'end' ? 'is-end' : undefined}>
          {cellContent(column, row)}
        </td>
      ))}
      {serviceColumns.map((column) => (
        <td key={column.id} className={column.align === 'end' ? 'is-end' : undefined}>
          {column.cell(row)}
        </td>
      ))}
    </tr>
  )
}

function GridStateNotice({
  kind,
  text,
  action,
}: Readonly<{
  kind: GridStateKind
  text: string
  action?: ReactNode
}>) {
  const Icon = GRID_STATE_ICONS[kind]
  const role = kind === 'error' ? 'alert' : 'status'
  return (
    <div
      className={`data-grid__state state-card state-card--stacked ${kind === 'error' ? 'state-card--error' : ''}`}
      role={role}
    >
      <Icon className={kind === 'loading' ? 'spin' : undefined} size={26} aria-hidden="true" />
      <div>
        <strong>{text}</strong>
        {action}
      </div>
    </div>
  )
}

/**
 * The footer of every table: remote cursor paging when the caller supplies it,
 * the local page count otherwise, and nothing for a remote page that carries
 * no pagination control. Split out so the renderer stays a flat read.
 */
function GridFooter<Row>({
  remote,
  resolved,
  page,
  pageCount,
  pageSize,
  onDispatch,
  pageSizeOptions,
  pending,
}: Readonly<{
  remote: Extract<DataGridProps<Row>, { mode: 'remote' }> | undefined
  resolved: GridResolved<Row>
  page: number
  pageCount: number
  pageSize: number
  onDispatch: (action: GridAction) => void
  pageSizeOptions: readonly number[] | undefined
  pending: boolean
}>) {
  const pageSizeProps = pageSizeOptions === undefined ? {} : { pageSizeOptions }
  if (remote?.pagination) {
    return (
      <GridPagination
        mode="remote"
        pending={pending}
        pageSize={pageSize}
        onPageSize={(size) => onDispatch({ type: 'pageSize', pageSize: size })}
        total={remote.total}
        shown={remote.pagination.shown}
        canPrevious={remote.pagination.canPrevious}
        canNext={remote.pagination.canNext}
        onPrevious={remote.pagination.onPrevious}
        onNext={remote.pagination.onNext}
        {...pageSizeProps}
      />
    )
  }
  if (resolved.kind === 'local') {
    return (
      <GridPagination
        pending={pending}
        page={page}
        pageCount={pageCount}
        pageSize={pageSize}
        total={resolved.total}
        onPage={(next) => onDispatch({ type: 'page', page: next })}
        onPageSize={(size) => onDispatch({ type: 'pageSize', pageSize: size })}
        {...pageSizeProps}
      />
    )
  }
  return null
}

/**
 * The one table renderer every migrated screen shares. Local mode receives the
 * full declared list and does all search/sort/filter/paging itself; remote mode
 * renders exactly the items, total and facets the server supplied and never
 * invents a row, a count or a facet value from the current page.
 *
 * The header — the one control that sorts and filters a column — is mounted in
 * every phase: loading, empty, error and ready. A reader who matched nothing
 * can still open a column menu and loosen the constraint; the body below the
 * header states that phase instead of the table disappearing.
 */
export function DataGrid<Row>(props: DataGridProps<Row>) {
  const { t } = useTranslation()
  const {
    rows,
    rowId,
    columns,
    serviceColumns,
    selection,
    state,
    onStateChange,
    ariaLabel,
    searchLabel,
    searchPlaceholder,
    emptyText,
    noMatchText,
    errorText,
    onRetry,
    pending = false,
    pageSizeOptions,
    className,
    toolbarExtras,
  } = props
  const remote = props.mode === 'remote' ? props : undefined

  const resolved = useMemo<GridResolved<Row>>(
    () =>
      remote
        ? {
            kind: 'remote' as const,
            rows: remote.rows,
            total: remote.total,
            pageCount: remote.total === null ? 1 : gridPageCount(remote.total, state.pageSize),
            facets: remote.facets ?? {},
          }
        : { kind: 'local' as const, ...resolveGridData(rows, columns, state, rowId) },
    [remote, rows, columns, state, rowId],
  )

  const dispatch = (action: GridAction) => {
    onStateChange((current) => reduceGridState(current, action))
  }

  const { rows: pageRows, pageCount } = resolved
  const total = resolved.total
  const page = remote ? state.page : Math.min(Math.max(1, state.page), pageCount)
  const hasQuery = state.search.trim() !== '' || Object.keys(state.filters).length > 0
  const trulyEmpty = total === 0 && !hasQuery
  const service = serviceColumns ?? []
  const facets: FacetQuery = facetQueryFor(remote?.serverFacets)
  const facetsByColumn = resolved.facets

  const phase = gridPhase(errorText, pending)

  return (
    <div className={`data-grid ${className ?? ''}`}>
      {errorText === undefined && (
        <GridToolbar
          columns={columns}
          state={state}
          facets={facetsByColumn}
          total={total}
          pending={pending}
          onSearch={(value) => dispatch({ type: 'search', value })}
          onRemoveFilter={(columnId, key) => dispatch({ type: 'toggleFilter', columnId, key })}
          onClearFilters={() => dispatch({ type: 'clearFilters' })}
          {...(searchLabel === undefined ? {} : { searchLabel })}
          {...(searchPlaceholder === undefined ? {} : { searchPlaceholder })}
          {...(toolbarExtras === undefined ? {} : { extras: toolbarExtras })}
        />
      )}
      <div className="data-grid__surface">
        <div className="data-grid__scroll">
          <table className="data-grid__table" aria-label={ariaLabel} aria-busy={pending}>
            <thead>
              <tr>
                {selection &&
                  (selection.mode === 'multiple' ? (
                    <GridSelectHead selection={selection} rows={pageRows} rowId={rowId} />
                  ) : (
                    <th scope="col" className="data-grid__select-head">
                      <span className="visually-hidden">{selection.headerLabel}</span>
                    </th>
                  ))}
                {columns.map((column) => (
                  <GridHeaderCell
                    key={column.id}
                    column={column}
                    facets={
                      remote?.serverFacets
                        ? facets
                        : { ...facets, options: facetsByColumn[column.id] ?? [] }
                    }
                    state={state}
                    dispatch={dispatch}
                  />
                ))}
                {service.map((column) => (
                  <th
                    key={column.id}
                    scope="col"
                    className="data-grid__service-head"
                    data-column={column.id}
                  >
                    {column.header ?? <span className="visually-hidden">{column.label}</span>}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {phase !== 'error' &&
                pageRows.map((row) => (
                  <GridBodyRow
                    key={rowId(row)}
                    row={row}
                    rowId={rowId(row)}
                    columns={columns}
                    serviceColumns={service}
                    selection={selection}
                  />
                ))}
            </tbody>
          </table>
        </div>
        {phase === 'error' && (
          <GridStateNotice
            kind="error"
            text={t('common.error')}
            action={
              <>
                <p>{errorText}</p>
                {onRetry && (
                  <Button size="sm" onClick={onRetry}>
                    {t('common.tryAgain')}
                  </Button>
                )}
              </>
            }
          />
        )}
        {phase === 'pending' && pageRows.length === 0 && (
          <GridStateNotice kind="loading" text={t('common.loading')} />
        )}
        {phase === 'ready' && pageRows.length === 0 && (
          <GridStateNotice
            kind={trulyEmpty ? 'empty' : 'noMatch'}
            text={trulyEmpty ? emptyText : noMatchText}
          />
        )}
      </div>
      {(phase === 'ready' || (phase === 'pending' && pageRows.length > 0)) && (
        <GridFooter
          remote={remote}
          resolved={resolved}
          page={page}
          pageCount={pageCount}
          pageSize={state.pageSize}
          onDispatch={dispatch}
          pageSizeOptions={pageSizeOptions}
          pending={phase === 'pending'}
        />
      )}
    </div>
  )
}
