import { act, renderHook, waitFor } from '@testing-library/react'
import { beforeEach, describe, expect, it } from 'vitest'

import {
  getTableStateStorageKey,
  isStorageRecord,
  parseNumberOption,
  parseStringOption,
  parseTableSort,
  useTableState,
  type TableSortState,
} from './useTableState'

const PAGE_SIZES = [25, 50] as const
const SORT_KEYS = ['name', 'status'] as const

type DemoTableState = {
  pageSize: (typeof PAGE_SIZES)[number]
  density: 'compact' | 'comfortable'
  sort: TableSortState<(typeof SORT_KEYS)[number]>
}

const DEFAULT_STATE: DemoTableState = {
  pageSize: 25,
  density: 'compact',
  sort: null,
}

function parseDemoTableState(raw: unknown): DemoTableState {
  const record = isStorageRecord(raw) ? raw : {}
  return {
    pageSize: parseNumberOption(record.pageSize, PAGE_SIZES, DEFAULT_STATE.pageSize),
    density: parseStringOption(
      record.density,
      ['compact', 'comfortable'] as const,
      DEFAULT_STATE.density,
    ),
    sort: parseTableSort(record.sort, SORT_KEYS, DEFAULT_STATE.sort),
  }
}

describe('useTableState', () => {
  beforeEach(() => {
    localStorage.clear()
  })

  it('restores valid table state and writes updates', async () => {
    const storageKey = getTableStateStorageKey('demo.table')
    localStorage.setItem(
      storageKey,
      JSON.stringify({
        pageSize: 50,
        density: 'comfortable',
        sort: { key: 'status', direction: 'desc' },
      }),
    )

    const { result } = renderHook(() =>
      useTableState<DemoTableState>({
        tableId: 'demo.table',
        defaultValue: DEFAULT_STATE,
        parse: parseDemoTableState,
      }),
    )

    expect(result.current[0]).toEqual({
      pageSize: 50,
      density: 'comfortable',
      sort: { key: 'status', direction: 'desc' },
    })

    act(() => {
      result.current[1]({
        pageSize: 25,
        density: 'compact',
        sort: { key: 'name', direction: 'asc' },
      })
    })

    await waitFor(() => {
      expect(JSON.parse(localStorage.getItem(storageKey) ?? 'null')).toEqual({
        pageSize: 25,
        density: 'compact',
        sort: { key: 'name', direction: 'asc' },
      })
    })
  })

  it('falls back when stored state is malformed', () => {
    localStorage.setItem(
      getTableStateStorageKey('demo.table'),
      JSON.stringify({ pageSize: 999, sort: { key: 'unknown', direction: 'asc' } }),
    )

    const { result } = renderHook(() =>
      useTableState<DemoTableState>({
        tableId: 'demo.table',
        defaultValue: DEFAULT_STATE,
        parse: parseDemoTableState,
      }),
    )

    expect(result.current[0]).toEqual(DEFAULT_STATE)
  })
})
