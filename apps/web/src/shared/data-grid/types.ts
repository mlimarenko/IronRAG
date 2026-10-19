import type { Dispatch, ReactNode, SetStateAction } from 'react'

/**
 * A value a grid column can sort, filter and search on. Dates travel as the
 * numeric epoch their accessor returns, so ordering is numeric rather than
 * lexicographic; `null` is a real, distinct value (a missing date, an issuer
 * the API did not name), never the empty string.
 */
export type GridValue = string | number | boolean | null

export type GridSortDirection = 'asc' | 'desc'

interface GridSort {
  columnId: string
  direction: GridSortDirection
}

/**
 * Selected facet values per column id, stored as canonical keys produced by
 * `gridFacetKey`. A key is typed, so the real string `"null"`, the boolean
 * `true` and the null value can never collide in the same bucket.
 */
type GridFilterSelection = Record<string, string[]>

/**
 * The whole controlled state of a grid: what is searched, sorted, filtered and
 * paged. Owned by the caller — the grid never keeps a second copy, and never
 * persists it (README: table *preferences* live in `useTableState`).
 */
export interface GridState {
  search: string
  sort: GridSort | null
  filters: GridFilterSelection
  page: number
  pageSize: number
}

/**
 * One data column. `value` is the typed raw value; `cell` the rendered cell;
 * `displayText` what a facet menu and a cell read by default; `searchText` what
 * the single whole-table search indexes. Every one of them defaults sensibly,
 * so a plain string column needs only `id`, `label` and `value`.
 */
export interface GridColumn<Row> {
  id: string
  label: string
  value: (row: Row) => GridValue
  cell?: (row: Row) => ReactNode
  displayText?: (row: Row) => string
  searchText?: (row: Row) => string
  /**
   * What local sorting orders this column by. It defaults to `value`; a column
   * whose stored/filter value is a stable raw key but whose reader sees a
   * translated label (a stage, a status) sets it so the order follows the
   * displayed label without changing the value's identity or the filter keys.
   */
  sortValue?: (row: Row) => string
  align?: 'start' | 'end'
}

/**
 * A non-data column: selection and actions. Exempt from sorting, filtering and
 * the global search by construction — an action label is not table data.
 */
export interface GridServiceColumn<Row> {
  id: string
  label: string
  header?: ReactNode
  cell: (row: Row) => ReactNode
  align?: 'start' | 'end'
}

export interface GridFacetOption {
  key: string
  value: GridValue
  /** The row's display text for this value; empty for the null bucket. */
  label: string
  /** Exact raw value for a hover title when the label is a formatted one. */
  title?: string
  count: number
}

/** Column id -> filterable values, either computed locally or supplied by the server. */
export type GridFacets = Record<string, GridFacetOption[]>

/**
 * One page of a server-computed column's values. `options` are the server's
 * own rows, never a slice the client derived from the page in view; `hasMore`
 * drives the menu's "load more"; `pending` and `error` describe the request
 * that is fetching this column, so a menu can say "loading" or "failed"
 * instead of claiming a column has no values.
 */
export interface GridFacetPage {
  options: GridFacetOption[]
  hasMore: boolean
  pending: boolean
  error: boolean
}

/**
 * A lazily-fetched, self-excluding facet source for remote mode. The grid
 * reports which column's menu opened, what the operator typed in it and when
 * they asked for another page; the caller owns the requests and the cache.
 */
export interface GridServerFacets {
  page: (columnId: string) => GridFacetPage | undefined
  onOpen: (columnId: string) => void
  onSearch: (columnId: string, query: string) => void
  onLoadMore: (columnId: string) => void
}

/** Cursor pagination of a remote list. There is no page number or page count. */
interface GridRemotePagination {
  canPrevious: boolean
  canNext: boolean
  onPrevious: () => void
  onNext: () => void
  /** The rows in view, e.g. `{ from: 1, to: 50 }`; null when nothing is shown. */
  shown: { from: number; to: number } | null
}

interface GridSelectionShared<Row> {
  isSelectable?: (row: Row) => boolean
  headerLabel: string
  rowLabel: (row: Row) => string
}

/**
 * Selection is one discriminated union, not a parallel prop. `single` is a
 * radio-like active row (the model picker); `multiple` is checkbox rows with a
 * header control that selects every selectable row on the page in one action
 * (the documents list). Both share selectability and labelling.
 */
export type GridSelection<Row> =
  | (GridSelectionShared<Row> & {
      mode?: 'single'
      activeRowId: string | null
      onActivate: (row: Row) => void
    })
  | (GridSelectionShared<Row> & {
      mode: 'multiple'
      selectedIds: ReadonlySet<string>
      onToggle: (row: Row) => void
      /** Header checkbox: select or clear every selectable row of the page. */
      onToggleAll: (checked: boolean, rows: Row[]) => void
    })

type DataGridMode =
  | { mode?: 'local' }
  | {
      mode: 'remote'
      /** Unknown totals stay null; the pagination must not invent one. */
      total: number | null
      /** Static server facets. Prefer `serverFacets` for lazy, searchable menus. */
      facets?: GridFacets
      serverFacets?: GridServerFacets
      pagination?: GridRemotePagination
    }

export type DataGridProps<Row> = {
  /** Local mode: the full declared list. Remote mode: the current server page. */
  rows: Row[]
  rowId: (row: Row) => string
  columns: GridColumn<Row>[]
  serviceColumns?: GridServiceColumn<Row>[]
  selection?: GridSelection<Row>
  state: GridState
  onStateChange: Dispatch<SetStateAction<GridState>>
  /** Accessible name of the `<table>`; every screen states what it lists. */
  ariaLabel: string
  searchLabel?: string
  searchPlaceholder?: string
  emptyText: string
  noMatchText: string
  errorText?: string
  onRetry?: () => void
  pending?: boolean
  pageSizeOptions?: readonly number[]
  className?: string
  /** Caller-owned controls rendered inside the toolbar, e.g. quick filters. */
  toolbarExtras?: ReactNode
} & DataGridMode
