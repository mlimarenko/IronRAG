import { render, screen, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import i18n from '@/shared/i18n'
import { DataGrid } from './DataGrid'
import { gridFacetKey } from './model'
import { createGridState, useDataGridState } from './useDataGridState'
import type { DataGridProps, GridColumn, GridState } from './types'

type Row = { id: string; name: string; amount: number | null }
const rows: Row[] = [
  { id: 'b', name: 'Beta', amount: 10 },
  { id: 'a', name: 'Alpha', amount: 2 },
  { id: 'c', name: 'Gamma', amount: null },
]
const columns: GridColumn<Row>[] = [
  { id: 'name', label: 'Name', value: (row) => row.name },
  { id: 'amount', label: 'Amount', value: (row) => row.amount },
]
const base = {
  rows,
  columns,
  rowId: (row: Row) => row.id,
  ariaLabel: 'Example records',
  emptyText: 'No records',
  noMatchText: 'No matching records',
}
function LocalGrid() {
  const grid = useDataGridState({ pageSize: 1 })
  return <DataGrid {...base} state={grid.state} onStateChange={grid.setState} />
}

describe('DataGrid', () => {
  beforeEach(async () => {
    await i18n.changeLanguage('en')
  })
  it('searches rows outside the first page with one global input', async () => {
    render(<LocalGrid />)
    expect(screen.getAllByRole('searchbox')).toHaveLength(1)
    expect(within(screen.getByRole('table')).queryByText('Gamma')).toBeNull()
    await userEvent.setup().type(screen.getByRole('searchbox'), 'Gamma')
    expect(within(screen.getByRole('table')).getByText('Gamma')).toBeVisible()
    expect(within(screen.getByRole('table')).queryByText('Beta')).toBeNull()
  })
  it('sorts from the column menu and exposes aria-sort', async () => {
    render(<LocalGrid />)
    const user = userEvent.setup()
    await user.click(
      within(screen.getByRole('columnheader', { name: /Amount/ })).getByRole('button'),
    )
    await user.click(screen.getByRole('menuitem', { name: /ascending/i }))
    expect(screen.getByRole('columnheader', { name: /Amount/ })).toHaveAttribute(
      'aria-sort',
      'ascending',
    )
    expect(within(screen.getByRole('table')).getByText('Alpha')).toBeVisible()
  })
  it('preserves both facet toggles before the controlled owner commits a render', async () => {
    const initial = createGridState()
    let queued = initial
    const onStateChange = (update: GridState | ((current: GridState) => GridState)) => {
      queued = typeof update === 'function' ? update(queued) : update
    }
    const { rerender } = render(
      <DataGrid {...base} state={initial} onStateChange={onStateChange} />,
    )
    const user = userEvent.setup()
    await user.click(within(screen.getByRole('columnheader', { name: /Name/ })).getByRole('button'))
    await user.click(screen.getByRole('menuitemcheckbox', { name: 'Alpha' }))
    await user.click(screen.getByRole('menuitemcheckbox', { name: 'Beta' }))
    expect(queued.filters.name).toEqual([gridFacetKey('Alpha'), gridFacetKey('Beta')])
    rerender(<DataGrid {...base} state={queued} onStateChange={onStateChange} />)
    for (const name of ['Alpha', 'Beta']) {
      expect(screen.getByRole('menuitemcheckbox', { name })).toHaveAttribute('aria-checked', 'true')
    }
  })
  it('keeps column controls available for an empty filter intersection', async () => {
    render(<LocalGrid />)
    const user = userEvent.setup()
    await user.type(screen.getByRole('searchbox'), 'No such row')
    expect(screen.getByText('No matching records')).toBeVisible()
    await user.click(
      within(screen.getByRole('columnheader', { name: /Amount/ })).getByRole('button'),
    )
    expect(screen.getByRole('menu')).toBeVisible()
    await user.keyboard('{Escape}')
  })
  it('does not process a remote server page as a complete local list', async () => {
    const changed = vi.fn()
    const state = createGridState({
      search: 'not in this page',
      sort: { columnId: 'amount', direction: 'asc' },
      page: 4,
      pageSize: 1,
    })
    render(
      <DataGrid
        {...base}
        mode="remote"
        total={40}
        facets={{ amount: [{ key: gridFacetKey(900), value: 900, label: '900', count: 12 }] }}
        state={state}
        onStateChange={changed}
      />,
    )
    const table = screen.getByRole('table')
    expect(within(table).getByText('Beta')).toBeVisible()
    expect(within(table).getByText('Alpha')).toBeVisible()
    await userEvent
      .setup()
      .click(within(screen.getByRole('columnheader', { name: /Amount/ })).getByRole('button'))
    expect(screen.getByRole('menuitemcheckbox', { name: /900/ })).toBeVisible()
    expect(screen.queryByRole('menuitemcheckbox', { name: /^2\s/ })).toBeNull()
    await userEvent.setup().click(screen.getByRole('menuitemcheckbox', { name: /900/ }))
    expect(changed).toHaveBeenCalledOnce()
    expect(changed.mock.calls[0]![0](state)).toEqual(
      expect.objectContaining({ page: 1, filters: { amount: [gridFacetKey(900)] } }),
    )
  })
  it('keeps the header controls mounted while pending and never fakes an empty body', () => {
    const props: DataGridProps<Row> = {
      ...base,
      rows: [],
      state: createGridState(),
      onStateChange: vi.fn(),
    }
    render(<DataGrid {...props} pending />)
    // The column menu is the control that loosens a constraint that matched
    // nothing, so the header stays mounted in every phase.
    expect(screen.getByRole('table')).toBeVisible()
    expect(
      within(screen.getByRole('columnheader', { name: /Amount/ })).getByRole('button'),
    ).toBeVisible()
    expect(screen.getByText('Loading')).toBeVisible()
    expect(screen.queryByText('No records')).toBeNull()
    // No fabricated zero count while the request is in flight.
    expect(screen.queryByText(/0 rows/)).toBeNull()
  })
  it('shows an error with retry while keeping the header reachable', async () => {
    const retry = vi.fn()
    const props: DataGridProps<Row> = {
      ...base,
      rows: [],
      state: createGridState(),
      onStateChange: vi.fn(),
    }
    render(<DataGrid {...props} errorText="Unavailable" onRetry={retry} />)
    expect(screen.getByRole('alert')).toHaveTextContent('Unavailable')
    expect(screen.getByRole('table')).toBeVisible()
    await userEvent.setup().click(screen.getByRole('button', { name: /Try again/i }))
    expect(retry).toHaveBeenCalledOnce()
  })
  it.each(['loading', 'error', 'empty', 'noMatch'] as const)(
    'keeps the %s boundary outside the horizontally scrolling table',
    (phase) => {
      render(
        <DataGrid
          {...base}
          rows={[]}
          state={createGridState({ search: phase === 'noMatch' ? 'missing' : '' })}
          onStateChange={vi.fn()}
          {...(phase === 'loading' ? { pending: true } : {})}
          {...(phase === 'error' ? { errorText: 'Unavailable', onRetry: vi.fn() } : {})}
        />,
      )
      const boundary = screen.getByRole(phase === 'error' ? 'alert' : 'status')
      expect(boundary.closest('.data-grid__scroll')).toBeNull()
      expect(boundary.closest('table')).toBeNull()
      expect(screen.getByRole('columnheader', { name: /Amount/ })).toBeVisible()
      if (phase === 'loading' || phase === 'error') {
        expect(screen.queryByText('0–0 of 0')).toBeNull()
        expect(screen.queryByRole('button', { name: 'Next' })).toBeNull()
      } else {
        expect(screen.getByText('0–0 of 0')).toBeVisible()
      }
    },
  )
  it('selects rows and the visible header control through the multiple-selection union', async () => {
    const onToggle = vi.fn()
    const onToggleAll = vi.fn()
    render(
      <DataGrid
        {...base}
        state={createGridState()}
        onStateChange={vi.fn()}
        selection={{
          mode: 'multiple',
          selectedIds: new Set(['b']),
          onToggle,
          onToggleAll,
          headerLabel: 'Select all',
          rowLabel: (row) => row.name,
        }}
      />,
    )
    const user = userEvent.setup()
    const beta = screen.getByRole('checkbox', { name: 'Beta' })
    expect(beta).toBeChecked()
    await user.click(screen.getByRole('checkbox', { name: 'Alpha' }))
    expect(onToggle).toHaveBeenCalledWith(expect.objectContaining({ id: 'a' }))
    await user.click(screen.getByRole('checkbox', { name: 'Select all' }))
    expect(onToggleAll).toHaveBeenCalledWith(true, rows)
  })
  it('renders cursor pagination without a page number or a fabricated total', async () => {
    const onNext = vi.fn()
    render(
      <DataGrid
        {...base}
        mode="remote"
        total={null}
        rows={rows.slice(0, 2)}
        state={createGridState()}
        onStateChange={vi.fn()}
        pagination={{
          canPrevious: false,
          canNext: true,
          onPrevious: vi.fn(),
          onNext,
          shown: { from: 1, to: 2 },
        }}
      />,
    )
    expect(screen.getByText('1–2')).toBeVisible()
    expect(screen.queryByText(/Page \d/)).toBeNull()
    await userEvent.setup().click(screen.getByRole('button', { name: /Next/i }))
    expect(onNext).toHaveBeenCalledOnce()
  })
  it.each(['next', 'pageSize'] as const)(
    'preserves remote %s focus through a pending transition with retained rows',
    async (focusedControl) => {
      const onNext = vi.fn()
      const props: DataGridProps<Row> = {
        ...base,
        mode: 'remote',
        total: 40,
        state: createGridState(),
        onStateChange: vi.fn(),
        pagination: {
          canPrevious: false,
          canNext: true,
          onPrevious: vi.fn(),
          onNext,
          shown: { from: 1, to: 3 },
        },
      }
      const view = render(<DataGrid {...props} />)
      const control =
        focusedControl === 'next'
          ? screen.getByRole('button', { name: 'Next' })
          : screen.getByRole('combobox', { name: 'Rows per page' })
      control.focus()
      view.rerender(<DataGrid {...props} pending />)
      expect(control).toHaveFocus()
      expect(control).toHaveAttribute('aria-disabled', 'true')
      expect(screen.queryByText('1–3 of 40')).toBeNull()
      if (focusedControl === 'next') {
        await userEvent.setup().keyboard('{Enter}')
      } else {
        await userEvent.setup().selectOptions(control, '50')
        expect(control).toHaveValue('25')
      }
      expect(onNext).not.toHaveBeenCalled()
      expect(props.onStateChange).not.toHaveBeenCalled()
      view.rerender(<DataGrid {...props} />)
      expect(control).toHaveFocus()
      expect(control).not.toHaveAttribute('aria-disabled', 'true')
      if (focusedControl === 'next') {
        await userEvent.setup().keyboard('{Enter}')
        expect(onNext).toHaveBeenCalledOnce()
      }
    },
  )
  it('keeps a server facet menu mounted with pending state instead of claiming no values', async () => {
    const onOpen = vi.fn()
    render(
      <DataGrid
        {...base}
        mode="remote"
        total={40}
        rows={rows.slice(0, 1)}
        state={createGridState({ filters: { amount: [gridFacetKey(900)] } })}
        onStateChange={vi.fn()}
        serverFacets={{
          page: (columnId) =>
            columnId === 'amount'
              ? { options: [], hasMore: false, pending: true, error: false }
              : undefined,
          onOpen,
          onSearch: vi.fn(),
          onLoadMore: vi.fn(),
        }}
      />,
    )
    await userEvent
      .setup()
      .click(within(screen.getByRole('columnheader', { name: /Amount/ })).getByRole('button'))
    expect(onOpen).toHaveBeenCalledWith('amount')
    expect(screen.getByText('Loading values…')).toBeVisible()
    expect(screen.queryByText('No values')).toBeNull()
    // The selected value stays removable while the server page is pending.
    expect(screen.getByRole('menuitemcheckbox', { name: '900' })).toBeVisible()
  })
  it('keeps an error and its retry reachable when another request is still pending', () => {
    render(
      <DataGrid
        {...base}
        state={createGridState()}
        onStateChange={vi.fn()}
        pending
        errorText="Unavailable"
        onRetry={vi.fn()}
      />,
    )
    expect(screen.getByRole('alert')).toHaveTextContent('Unavailable')
    expect(screen.getByRole('button', { name: /Try again/i })).toBeVisible()
    expect(screen.queryByText('Loading')).toBeNull()
  })
  it('removes a selected value via its chip without changing the global search', async () => {
    render(<LocalGrid />)
    const user = userEvent.setup()
    await user.click(
      within(screen.getByRole('columnheader', { name: /Amount/ })).getByRole('button'),
    )
    await user.click(screen.getByRole('menuitemcheckbox', { name: '2' }))
    await user.keyboard('{Escape}')
    expect(within(screen.getByRole('table')).getByText('Alpha')).toBeVisible()
    await user.click(screen.getByRole('button', { name: /Remove filter.*Amount.*2/i }))
    expect(within(screen.getByRole('table')).getByText('Beta')).toBeVisible()
    expect(screen.getByRole('searchbox')).toHaveValue('')
  })
  it('preserves disabled model selection and keeps actions out of column menus', async () => {
    const selected = vi.fn()
    render(
      <DataGrid
        {...base}
        state={createGridState()}
        onStateChange={vi.fn()}
        selection={{
          activeRowId: null,
          onActivate: selected,
          isSelectable: (row) => row.id !== 'b',
          headerLabel: 'Selection',
          rowLabel: (row) => row.name,
        }}
        serviceColumns={[
          { id: 'actions', label: 'Actions', cell: () => <button type="button">Inspect</button> },
        ]}
      />,
    )
    await userEvent.setup().click(screen.getByRole('button', { name: 'Beta' }))
    expect(selected).not.toHaveBeenCalled()
    await userEvent.setup().click(screen.getByRole('button', { name: 'Alpha' }))
    expect(selected).toHaveBeenCalledOnce()
    expect(
      within(screen.getByRole('columnheader', { name: 'Actions' })).queryByRole('button'),
    ).toBeNull()
  })
})
