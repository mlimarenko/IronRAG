import { useState } from 'react'
import type { GridState } from './types'

const DEFAULT_GRID_PAGE_SIZE = 25

/** A fresh grid state, overridable per screen. Nothing is persisted here. */
export function createGridState(overrides: Partial<GridState> = {}): GridState {
  return {
    search: '',
    sort: null,
    filters: {},
    page: 1,
    pageSize: DEFAULT_GRID_PAGE_SIZE,
    ...overrides,
  }
}

/**
 * The one owner of grid state for an uncontrolled screen. The transitions live
 * in the grid's reducer; this hook stores only its controlled result.
 * Nothing is written to storage: table *preferences* are
 * `useTableState`'s job, query state is not a preference.
 */
export function useDataGridState(initial?: Partial<GridState>) {
  const [state, setState] = useState<GridState>(() => createGridState(initial))
  return { state, setState }
}
