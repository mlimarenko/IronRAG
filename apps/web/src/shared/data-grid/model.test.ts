import { describe, expect, it } from 'vitest'
import {
  buildGridFacets,
  gridFacetKey,
  gridFacetValue,
  reduceGridState,
  resolveGridData,
  sortGridRows,
} from './model'
import { createGridState } from './useDataGridState'
import type { GridColumn, GridValue } from './types'

type Row = { id: string; name: string; amount: number | null; active: boolean }
const rows: Row[] = [
  { id: 'b', name: 'Beta', amount: 10, active: true },
  { id: 'a', name: 'Alpha', amount: 2, active: false },
  { id: 'c', name: 'Gamma', amount: null, active: true },
  { id: 'd', name: 'Delta', amount: 2, active: true },
]
const columns: GridColumn<Row>[] = [
  { id: 'name', label: 'Name', value: (row) => row.name },
  { id: 'amount', label: 'Amount', value: (row) => row.amount },
  {
    id: 'active',
    label: 'Status',
    value: (row) => row.active,
    displayText: (row) => (row.active ? 'Enabled' : 'Disabled'),
  },
]
const rowId = (row: Row) => row.id

describe('typed grid facets', () => {
  it.each([null, 'null', '∅', 'number:2', '', 2, true, false] satisfies GridValue[])(
    'round trips %s without type collisions',
    (value) => {
      expect(gridFacetValue(gridFacetKey(value))).toBe(value)
    },
  )
  it('keeps values with identical string representations in distinct buckets', () => {
    expect(new Set([null, 'null', 2, '2', true, 'true'].map(gridFacetKey)).size).toBe(6)
  })
  it.each(['number:', 'number:NaN', 'number:Infinity', 'boolean:yes', 'raw'])(
    'rejects invalid state keys: %s',
    (key) => {
      expect(() => gridFacetValue(key)).toThrow('Invalid grid facet key')
    },
  )
  it.each([NaN, Infinity, -Infinity])('rejects non-finite raw numbers', (value) => {
    expect(() => gridFacetKey(value)).toThrow('must be finite')
  })
})

describe('whole-set grid processing', () => {
  it('sorts numbers numerically, ties by stable ID, and leaves NULL last in either direction', () => {
    expect(
      sortGridRows(rows, columns, { columnId: 'amount', direction: 'asc' }, rowId).map(rowId),
    ).toEqual(['a', 'd', 'b', 'c'])
    expect(
      sortGridRows(rows, columns, { columnId: 'amount', direction: 'desc' }, rowId).map(rowId),
    ).toEqual(['b', 'a', 'd', 'c'])
    expect(rows.map(rowId)).toEqual(['b', 'a', 'c', 'd'])
  })
  it('applies OR within values and AND across columns before pagination', () => {
    const state = createGridState({
      pageSize: 1,
      filters: { amount: [gridFacetKey(2), gridFacetKey(10)], active: [gridFacetKey(true)] },
    })
    const result = resolveGridData(rows, columns, state, rowId)
    expect(result.total).toBe(2)
    expect(result.rows.map(rowId)).toEqual(['b'])
    expect(result.pageCount).toBe(2)
  })
  it.each(['  gAMma  ', '10', 'disabled'])(
    'searches all displayed and raw values across the complete set: %s',
    (search) => {
      const result = resolveGridData(rows, columns, createGridState({ search, pageSize: 1 }), rowId)
      expect(result.total).toBe(1)
    },
  )
  it('does not narrow available facet values to the current page or to the column itself', () => {
    const state = createGridState({
      pageSize: 1,
      filters: { amount: [gridFacetKey(2)], active: [gridFacetKey(true)] },
    })
    const facets = buildGridFacets(rows, columns, state)
    expect(facets.amount?.map((option) => [option.value, option.count])).toEqual([
      [2, 1],
      [10, 1],
      [null, 1],
    ])
    expect(facets.active?.map((option) => [option.value, option.count])).toEqual([
      [false, 1],
      [true, 1],
    ])
  })
  it('applies global search to facets and preserves a selected zero-count value for clearing', () => {
    const facets = buildGridFacets(
      rows,
      columns,
      createGridState({ search: 'Beta', filters: { amount: [gridFacetKey(2)] } }),
    )
    expect(facets.amount).toEqual(
      expect.arrayContaining([
        expect.objectContaining({ value: 10, count: 1 }),
        expect.objectContaining({ value: 2, count: 0 }),
      ]),
    )
  })
  it('clamps a stale page after the source shrinks without claiming the dataset is empty', () => {
    const result = resolveGridData(rows, columns, createGridState({ page: 50, pageSize: 2 }), rowId)
    expect(result.rows.map(rowId)).toEqual(['c', 'd'])
    expect(result.total).toBe(4)
  })
})

describe('controlled grid transitions', () => {
  it('resets pagination for search, sort, filter and page-size changes without mutating inputs', () => {
    const state = createGridState({ page: 3 })
    const actions = [
      { type: 'search', value: 'Beta' },
      { type: 'sort', columnId: 'amount', direction: 'desc' },
      { type: 'toggleFilter', columnId: 'amount', key: gridFacetKey(2) },
      { type: 'pageSize', pageSize: 50 },
    ] as const
    for (const action of actions) expect(reduceGridState(state, action).page).toBe(1)
    expect(state.page).toBe(3)
    expect(state.filters).toEqual({})
  })
  it('toggle removal deletes an empty constraint and leaves other columns alone', () => {
    const state = createGridState({
      filters: { amount: [gridFacetKey(2)], active: [gridFacetKey(true)] },
    })
    expect(
      reduceGridState(state, { type: 'toggleFilter', columnId: 'amount', key: gridFacetKey(2) })
        .filters,
    ).toEqual({ active: [gridFacetKey(true)] })
  })
})

describe('sortValue', () => {
  it.each(['asc', 'desc'] as const)(
    'keeps raw NULL last before formatting sort labels (%s)',
    (direction) => {
      const column: GridColumn<{ id: string; value: string | null }> = {
        id: 'nullable',
        label: 'Nullable',
        value: (row) => row.value,
        sortValue: (row) => (row.value === null ? '' : row.value),
      }
      const source = [
        { id: 'null', value: null },
        { id: 'known', value: 'Visible' },
      ]
      expect(
        sortGridRows(source, [column], { columnId: 'nullable', direction }, (row) => row.id).map(
          (row) => row.id,
        ),
      ).toEqual(['known', 'null'])
    },
  )
  type StageRow = { id: string; stage: string }
  const stageRows: StageRow[] = [
    { id: 'chunk', stage: 'chunk_content' },
    { id: 'vector', stage: 'embed_chunk' },
  ]
  const comparator = (label: (row: StageRow) => string): GridColumn<StageRow> => ({
    id: 'stage',
    label: 'Stage',
    value: (row) => row.stage,
    displayText: label,
    sortValue: label,
  })

  it('orders by the display label, not the stored raw value, in both directions', () => {
    // Raw tokens sort chunk_content < embed_chunk; their Russian labels invert
    // that ('Нарезка…' > 'Векторизация…'), so a value-only sort would
    // contradict the visible order.
    const labels: Record<string, string> = {
      chunk_content: 'Нарезка на сегменты',
      embed_chunk: 'Векторизация сегментов',
    }
    const column = comparator((row) => labels[row.stage] ?? row.stage)
    expect(
      sortGridRows(
        stageRows,
        [column],
        { columnId: 'stage', direction: 'asc' },
        (row) => row.id,
      ).map((row) => row.id),
    ).toEqual(['vector', 'chunk'])
    expect(
      sortGridRows(
        stageRows,
        [column],
        { columnId: 'stage', direction: 'desc' },
        (row) => row.id,
      ).map((row) => row.id),
    ).toEqual(['chunk', 'vector'])
  })

  it('falls back to the raw value when no sortValue is supplied', () => {
    const plain: GridColumn<StageRow>[] = [
      { id: 'stage', label: 'Stage', value: (row) => row.stage },
    ]
    expect(
      sortGridRows(stageRows, plain, { columnId: 'stage', direction: 'asc' }, (row) => row.id).map(
        (row) => row.id,
      ),
    ).toEqual(['chunk', 'vector'])
  })
})
