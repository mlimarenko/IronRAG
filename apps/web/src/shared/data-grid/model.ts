import type {
  GridColumn,
  GridFacetOption,
  GridFacets,
  GridSortDirection,
  GridState,
  GridValue,
} from './types'

const STRING_PREFIX = 'string:'
const NUMBER_PREFIX = 'number:'
const BOOLEAN_PREFIX = 'boolean:'
const NULL_KEY = 'null'

/**
 * Canonical, typed key for a facet value. The type prefix is what keeps the
 * real string `"null"` out of the null bucket, `"true"` out of the boolean one
 * and a string like `"number:5"` out of the numeric one.
 */
export function gridFacetKey(value: GridValue): string {
  if (value === null) return NULL_KEY
  if (typeof value === 'string') return `${STRING_PREFIX}${value}`
  if (typeof value === 'number') {
    if (!Number.isFinite(value)) throw new Error('Grid facet numbers must be finite')
    return `${NUMBER_PREFIX}${value}`
  }
  return `${BOOLEAN_PREFIX}${value}`
}

/** Inverse of {@link gridFacetKey}; invalid state is never a second key format. */
export function gridFacetValue(key: string): GridValue {
  if (key === NULL_KEY) return null
  if (key.startsWith(STRING_PREFIX)) return key.slice(STRING_PREFIX.length)
  if (key.startsWith(NUMBER_PREFIX)) {
    const value = Number(key.slice(NUMBER_PREFIX.length))
    if (Number.isFinite(value) && gridFacetKey(value) === key) return value
  }
  if (key === `${BOOLEAN_PREFIX}true`) return true
  if (key === `${BOOLEAN_PREFIX}false`) return false
  throw new Error(`Invalid grid facet key: ${key}`)
}

/** What a cell and a facet option read by default. Null reads as empty. */
export function gridColumnDisplayText<Row>(column: GridColumn<Row>, row: Row): string {
  if (column.displayText) return column.displayText(row)
  const value = column.value(row)
  return value === null ? '' : String(value)
}

/**
 * What the single whole-table search indexes for one column. Defaults to the
 * displayed text *plus* the raw value, so a number ("262144"), a boolean and a
 * numeric date are all searchable, not only prose. The raw value is never a
 * secret: descriptors only ever expose displayed metadata.
 */
function gridColumnSearchText<Row>(column: GridColumn<Row>, row: Row): string {
  if (column.searchText) return column.searchText(row)
  const display = gridColumnDisplayText(column, row)
  const value = column.value(row)
  if (value === null) return display
  return `${display} ${String(value)}`.trim()
}

function rowSearchText<Row>(columns: GridColumn<Row>[], row: Row): string {
  return columns
    .map((column) => gridColumnSearchText(column, row))
    .join(' ')
    .toLowerCase()
}

function gridRowMatchesSearch<Row>(columns: GridColumn<Row>[], row: Row, needle: string): boolean {
  if (!needle) return true
  return rowSearchText(columns, row).includes(needle)
}

/**
 * OR inside one column's selected values, AND across columns. A column with no
 * selection constrains nothing. `excludedColumnId` supports the self-excluding
 * facets: a column's own menu lists the values the *other* filters leave.
 */
function gridRowMatchesFilters<Row>(
  columns: GridColumn<Row>[],
  row: Row,
  filters: GridState['filters'],
  excludedColumnId?: string,
): boolean {
  for (const column of columns) {
    if (column.id === excludedColumnId) continue
    const selected = filters[column.id]
    if (!selected || selected.length === 0) continue
    if (!selected.includes(gridFacetKey(column.value(row)))) return false
  }
  return true
}

function filterGridRows<Row>(
  rows: Row[],
  columns: GridColumn<Row>[],
  state: GridState,
  excludedColumnId?: string,
): Row[] {
  const needle = state.search.trim().toLowerCase()
  return rows.filter(
    (row) =>
      gridRowMatchesSearch(columns, row, needle) &&
      gridRowMatchesFilters(columns, row, state.filters, excludedColumnId),
  )
}

/** Null sorts last in both directions; numbers and booleans compare by value. */
function compareGridValues(left: GridValue, right: GridValue): number {
  if (left === null) return right === null ? 0 : 1
  if (right === null) return -1
  if (typeof left === 'number' && typeof right === 'number') return left - right
  if (typeof left === 'boolean' && typeof right === 'boolean') return Number(left) - Number(right)
  return String(left).localeCompare(String(right))
}

/** What local sorting orders a row by: the column's sort text, or its raw value. */
function gridColumnSortValue<Row>(column: GridColumn<Row>, row: Row): GridValue {
  const value = column.value(row)
  if (value === null) return null
  return column.sortValue ? column.sortValue(row) : value
}

export function sortGridRows<Row>(
  rows: Row[],
  columns: GridColumn<Row>[],
  sort: GridState['sort'],
  rowId: (row: Row) => string,
): Row[] {
  if (!sort) return rows
  const column = columns.find((candidate) => candidate.id === sort.columnId)
  if (!column) return rows
  const indexed = rows.map((row, index) => ({ index, row }))
  indexed.sort((left, right) => {
    const a = gridColumnSortValue(column, left.row)
    const b = gridColumnSortValue(column, right.row)
    const compared = compareGridValues(a, b)
    if (a !== null && b !== null && compared !== 0) {
      return sort.direction === 'asc' ? compared : -compared
    }
    if (compared !== 0) return compared
    const byId = rowId(left.row).localeCompare(rowId(right.row))
    if (byId !== 0) return byId
    return left.index - right.index
  })
  return indexed.map((entry) => entry.row)
}

function compareGridFacetOptions(left: GridFacetOption, right: GridFacetOption): number {
  const compared = compareGridValues(left.value, right.value)
  if (compared !== 0) return compared
  return left.key.localeCompare(right.key)
}

function buildGridFacetOptions<Row>(
  rows: Row[],
  column: GridColumn<Row>,
  selectedKeys: readonly string[],
): GridFacetOption[] {
  const buckets = new Map<string, GridFacetOption>()
  for (const row of rows) {
    const value = column.value(row)
    const key = gridFacetKey(value)
    const bucket = buckets.get(key)
    if (bucket) {
      bucket.count += 1
      continue
    }
    buckets.set(key, { key, value, label: gridColumnDisplayText(column, row), count: 1 })
  }
  const options = [...buckets.values()].sort(compareGridFacetOptions)
  for (const key of selectedKeys) {
    if (options.some((option) => option.key === key)) continue
    // A value selected before the current rows stopped carrying it stays in the
    // menu, so the operator can still see and clear the constraint.
    const value = gridFacetValue(key)
    options.push({ key, value, label: value === null ? '' : String(value), count: 0 })
  }
  return options
}

/**
 * Facet options for every filterable column, each computed over the full
 * declared list narrowed by the global search and every *other* filter. The
 * numbers a menu shows are therefore the ones choosing that value would yield,
 * and never a value invented from a single server page.
 */
export function buildGridFacets<Row>(
  rows: Row[],
  columns: GridColumn<Row>[],
  state: GridState,
): GridFacets {
  const facets: GridFacets = {}
  for (const column of columns) {
    const subset = filterGridRows(rows, columns, state, column.id)
    facets[column.id] = buildGridFacetOptions(subset, column, state.filters[column.id] ?? [])
  }
  return facets
}

export function gridPageCount(total: number, pageSize: number): number {
  if (pageSize <= 0) return 1
  return Math.max(1, Math.ceil(total / pageSize))
}

function clampGridPage(page: number, pageCount: number): number {
  return Math.min(Math.max(1, page), pageCount)
}

function gridPageRows<Row>(rows: Row[], page: number, pageSize: number): Row[] {
  const safeSize = Math.max(1, pageSize)
  const start = (clampGridPage(page, gridPageCount(rows.length, safeSize)) - 1) * safeSize
  return rows.slice(start, start + safeSize)
}

export interface ResolvedGridData<Row> {
  /** The rows of the requested page. */
  rows: Row[]
  /** Size of the full filtered set, not of one page. */
  total: number
  pageCount: number
  facets: GridFacets
}

/** The whole local-mode pipeline: search, filter, sort, page, facets. */
export function resolveGridData<Row>(
  rows: Row[],
  columns: GridColumn<Row>[],
  state: GridState,
  rowId: (row: Row) => string,
): ResolvedGridData<Row> {
  const filtered = filterGridRows(rows, columns, state)
  const sorted = sortGridRows(filtered, columns, state.sort, rowId)
  const total = sorted.length
  const pageCount = gridPageCount(total, state.pageSize)
  return {
    rows: gridPageRows(sorted, state.page, state.pageSize),
    total,
    pageCount,
    facets: buildGridFacets(rows, columns, state),
  }
}

export interface GridFilterChip {
  columnId: string
  columnLabel: string
  key: string
  value: GridValue
  label: string
}

/**
 * The active-filter chips the toolbar shows, in column order. A chip's label is
 * the same display text the facet menu offered, so a translated status reads
 * back translated rather than as its storage token.
 */
export function gridFilterChips<Row>(
  columns: GridColumn<Row>[],
  state: GridState,
  facets: GridFacets,
): GridFilterChip[] {
  const chips: GridFilterChip[] = []
  for (const column of columns) {
    for (const key of state.filters[column.id] ?? []) {
      const option = facets[column.id]?.find((candidate) => candidate.key === key)
      const value = gridFacetValue(key)
      chips.push({
        columnId: column.id,
        columnLabel: column.label,
        key,
        value,
        label: option?.label ?? (value === null ? '' : String(value)),
      })
    }
  }
  return chips
}

/** Every way the controlled state can move. */
export type GridAction =
  | { type: 'search'; value: string }
  | { type: 'sort'; columnId: string; direction: GridSortDirection }
  | { type: 'clearSort'; columnId: string }
  | { type: 'toggleFilter'; columnId: string; key: string }
  | { type: 'clearColumnFilter'; columnId: string }
  | { type: 'clearFilters' }
  | { type: 'page'; page: number }
  | { type: 'pageSize'; pageSize: number }

function toggleFilterKey(selected: string[], key: string): string[] {
  return selected.includes(key)
    ? selected.filter((candidate) => candidate !== key)
    : [...selected, key]
}

/**
 * The single implementation of a grid-state transition, shared by the state
 * hook and the controlled renderer so they can never drift. Anything that
 * changes which rows match returns to page one.
 */
export function reduceGridState(state: GridState, action: GridAction): GridState {
  switch (action.type) {
    case 'search':
      return { ...state, search: action.value, page: 1 }
    case 'sort':
      return { ...state, sort: { columnId: action.columnId, direction: action.direction }, page: 1 }
    case 'clearSort':
      return state.sort?.columnId === action.columnId ? { ...state, sort: null, page: 1 } : state
    case 'toggleFilter': {
      const next = toggleFilterKey(state.filters[action.columnId] ?? [], action.key)
      const filters = { ...state.filters }
      if (next.length === 0) {
        delete filters[action.columnId]
      } else {
        filters[action.columnId] = next
      }
      return { ...state, filters, page: 1 }
    }
    case 'clearColumnFilter': {
      if (!state.filters[action.columnId]) return state
      const filters = { ...state.filters }
      delete filters[action.columnId]
      return { ...state, filters, page: 1 }
    }
    case 'clearFilters':
      return { ...state, filters: {}, page: 1 }
    case 'page':
      return { ...state, page: Math.max(1, action.page) }
    case 'pageSize':
      return { ...state, pageSize: action.pageSize, page: 1 }
  }
}
