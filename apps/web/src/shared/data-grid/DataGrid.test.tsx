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

describe('DataGrid rendering and dispatch', () => {
  beforeEach(async () => {
    await i18n.changeLanguage('en')
  })

  it('renders a custom cell, a service column and both end alignments', () => {
    render(
      <DataGrid
        {...base}
        columns={[
          {
            id: 'name',
            label: 'Name',
            value: (row) => row.name,
            cell: (row) => <em>{row.name}</em>,
          },
          { id: 'amount', label: 'Amount', value: (row) => row.amount, align: 'end' },
        ]}
        serviceColumns={[
          { id: 'actions', label: 'Actions', align: 'end', cell: () => <a href="#open">Open</a> },
        ]}
        state={createGridState()}
        onStateChange={vi.fn()}
      />,
    )
    const row = within(screen.getByRole('table')).getAllByRole('row')[1]!
    const [nameCell, amountCell, actionCell] = within(row).getAllByRole('cell')
    expect(nameCell?.querySelector('em')).toHaveTextContent('Beta')
    expect(nameCell).not.toHaveClass('is-end')
    expect(amountCell).toHaveClass('is-end')
    expect(actionCell).toHaveClass('is-end')
    expect(within(row).getByRole('link', { name: 'Open' })).toBeVisible()
  })

  it('activates the clicked row in single mode and toggles it in multiple mode', async () => {
    const onActivate = vi.fn()
    const onToggle = vi.fn()
    const { rerender } = render(
      <DataGrid
        {...base}
        state={createGridState()}
        onStateChange={vi.fn()}
        selection={{
          activeRowId: 'a',
          onActivate,
          isSelectable: (row) => row.id !== 'c',
          headerLabel: 'Selection',
          rowLabel: (row) => row.name,
        }}
      />,
    )
    const user = userEvent.setup()
    const betaRow = within(screen.getByRole('table')).getByText('Beta').closest('tr')!
    const alphaRow = within(screen.getByRole('table')).getByText('Alpha').closest('tr')!
    const gammaRow = within(screen.getByRole('table')).getByText('Gamma').closest('tr')!
    expect(alphaRow).toHaveAttribute('aria-selected', 'true')
    expect(gammaRow).toHaveClass('is-blocked')

    await user.click(gammaRow)
    expect(onActivate).not.toHaveBeenCalled()
    await user.click(betaRow)
    expect(onActivate).toHaveBeenCalledWith(expect.objectContaining({ id: 'b' }))

    rerender(
      <DataGrid
        {...base}
        state={createGridState()}
        onStateChange={vi.fn()}
        selection={{
          mode: 'multiple',
          selectedIds: new Set(['a']),
          onToggle,
          onToggleAll: vi.fn(),
          headerLabel: 'Select all',
          rowLabel: (row) => row.name,
        }}
      />,
    )
    await user.click(within(screen.getByRole('table')).getByText('Beta').closest('tr')!)
    expect(onToggle).toHaveBeenCalledWith(expect.objectContaining({ id: 'b' }))
    expect(screen.getByRole('checkbox', { name: 'Select all' })).toBePartiallyChecked()
  })

  it('moves the local page and page size through the reducer', async () => {
    render(<LocalGrid />)
    const user = userEvent.setup()
    expect(screen.getByText('1–1 of 3')).toBeVisible()
    await user.click(screen.getByRole('button', { name: 'Next' }))
    expect(screen.getByText('2–2 of 3')).toBeVisible()
    await user.selectOptions(screen.getByRole('combobox', { name: 'Rows per page' }), '50')
    expect(screen.getByText('1–3 of 3')).toBeVisible()
  })

  it('dispatches a remote page-size change and honours caller labels and extras', async () => {
    const changed = vi.fn()
    render(
      <DataGrid
        {...base}
        mode="remote"
        total={40}
        state={createGridState()}
        onStateChange={changed}
        searchLabel="Find a record"
        searchPlaceholder="Filter the list"
        toolbarExtras={<span>Quick filters</span>}
        pageSizeOptions={[10, 20]}
        pagination={{
          canPrevious: false,
          canNext: true,
          onPrevious: vi.fn(),
          onNext: vi.fn(),
          shown: { from: 1, to: 3 },
        }}
      />,
    )
    const search = screen.getByRole('searchbox', { name: 'Find a record' })
    expect(search).toHaveAttribute('placeholder', 'Filter the list')
    expect(screen.getByText('Quick filters')).toBeVisible()
    await userEvent
      .setup()
      .selectOptions(screen.getByRole('combobox', { name: 'Rows per page' }), '20')
    expect(changed).toHaveBeenCalledOnce()
    expect(changed.mock.calls[0]![0](createGridState())).toEqual(
      expect.objectContaining({ pageSize: 20, page: 1 }),
    )
  })

  it('clears every active filter from the toolbar chips', async () => {
    render(<LocalGrid />)
    const user = userEvent.setup()
    await user.click(
      within(screen.getByRole('columnheader', { name: /Amount/ })).getByRole('button'),
    )
    await user.click(screen.getByRole('menuitemcheckbox', { name: '2' }))
    await user.keyboard('{Escape}')
    expect(screen.getByRole('button', { name: /Remove filter.*Amount.*2/i })).toBeVisible()
    await user.click(screen.getByRole('button', { name: 'Clear filters' }))
    expect(screen.queryByRole('button', { name: /Remove filter/ })).toBeNull()
    expect(within(screen.getByRole('table')).getByText('Beta')).toBeVisible()
  })

  it('clears the sort and one column filter from the open column menu', async () => {
    render(<LocalGrid />)
    const user = userEvent.setup()
    const amountHeader = () => screen.getByRole('columnheader', { name: /Amount/ })
    const openAmountMenu = () => user.click(within(amountHeader()).getByRole('button'))

    await openAmountMenu()
    expect(screen.getByRole('menuitem', { name: 'Clear sort' })).toHaveAttribute(
      'aria-disabled',
      'true',
    )
    await user.click(screen.getByRole('menuitem', { name: 'Sort descending' }))
    expect(amountHeader()).toHaveAttribute('aria-sort', 'descending')
    await openAmountMenu()
    await user.click(screen.getByRole('menuitem', { name: 'Clear sort' }))
    expect(amountHeader()).not.toHaveAttribute('aria-sort')

    await openAmountMenu()
    await user.click(screen.getByRole('menuitemcheckbox', { name: '2' }))
    await user.keyboard('{Escape}')
    expect(amountHeader().querySelector('.data-grid__head-filter')).not.toBeNull()
    await openAmountMenu()
    await user.click(screen.getByRole('menuitem', { name: 'Clear column' }))
    expect(amountHeader().querySelector('.data-grid__head-filter')).toBeNull()
    expect(screen.queryByRole('button', { name: /Remove filter/ })).toBeNull()
  })

  it('narrows a local facet list by the typed value and reports no matches', async () => {
    render(<LocalGrid />)
    const user = userEvent.setup()
    await user.click(within(screen.getByRole('columnheader', { name: /Name/ })).getByRole('button'))
    const filterSearch = within(screen.getByRole('menu')).getByRole('searchbox')
    await user.type(filterSearch, 'bet')
    expect(filterSearch).toHaveValue('bet')
    // Typing narrows the list without letting Radix typeahead steal the focus.
    expect(filterSearch).toHaveFocus()
    await user.keyboard('{ArrowDown}')
    expect(filterSearch).toHaveValue('bet')
    expect(screen.getByRole('menuitemcheckbox', { name: 'Beta' })).toBeVisible()
    expect(screen.queryByRole('menuitemcheckbox', { name: 'Alpha' })).toBeNull()
    await user.clear(filterSearch)
    await user.type(filterSearch, 'zzz')
    expect(screen.getByText('No values')).toBeVisible()
    expect(screen.getByRole('menuitem', { name: 'Clear column' })).toHaveAttribute(
      'aria-disabled',
      'true',
    )
  })

  it('matches the NULL bucket by its displayed label in a local facet list', async () => {
    render(<LocalGrid />)
    const user = userEvent.setup()
    await user.click(
      within(screen.getByRole('columnheader', { name: /Amount/ })).getByRole('button'),
    )
    await user.type(within(screen.getByRole('menu')).getByRole('searchbox'), '∅')
    expect(screen.getByRole('menuitemcheckbox', { name: '∅' })).toBeVisible()
    expect(screen.queryByRole('menuitemcheckbox', { name: '2' })).toBeNull()
  })

  it('drives a server facet menu through search, load more, failure and pending', async () => {
    const onSearch = vi.fn()
    const onLoadMore = vi.fn()
    let page = {
      options: [{ key: gridFacetKey(900), value: 900, label: '900', count: 4 }],
      hasMore: true,
      pending: false,
      error: false,
    }
    const serverFacets = { page: () => page, onOpen: vi.fn(), onSearch, onLoadMore }
    const props = {
      ...base,
      mode: 'remote' as const,
      total: 40,
      rows: rows.slice(0, 1),
      state: createGridState({ filters: { amount: [gridFacetKey(null)] } }),
      onStateChange: vi.fn(),
      serverFacets,
    }
    const view = render(<DataGrid {...props} />)
    const user = userEvent.setup()
    await user.click(
      within(screen.getByRole('columnheader', { name: /Amount/ })).getByRole('button'),
    )
    const menuSearch = within(screen.getByRole('menu')).getByRole('searchbox')
    await user.type(menuSearch, '9')
    expect(onSearch).toHaveBeenCalledWith('amount', '9')
    // A selected value the server page did not return stays visible.
    expect(screen.getByRole('menuitemcheckbox', { name: '∅' })).toBeVisible()

    await user.click(screen.getByRole('menuitem', { name: 'Load more values' }))
    expect(onLoadMore).toHaveBeenCalledWith('amount')
    expect(screen.getByRole('menu')).toBeVisible()

    page = { options: [], hasMore: true, pending: true, error: false }
    view.rerender(<DataGrid {...props} />)
    expect(screen.getByText('Loading values…')).toBeVisible()
    expect(screen.getByRole('menuitem', { name: 'Load more values' })).toHaveAttribute(
      'aria-disabled',
      'true',
    )

    page = { options: [], hasMore: false, pending: false, error: true }
    view.rerender(<DataGrid {...props} state={createGridState()} />)
    expect(screen.getByText('Values unavailable')).toBeVisible()
    expect(screen.queryByText('No values')).toBeNull()

    page = {
      options: [{ key: gridFacetKey(900), value: 900, label: '900', count: 4 }],
      hasMore: false,
      pending: false,
      error: true,
    }
    view.rerender(<DataGrid {...props} state={createGridState()} />)
    expect(screen.queryByText('Values unavailable')).toBeNull()
    expect(screen.getByRole('menuitemcheckbox', { name: /900/ })).toBeVisible()
  })

  it('keeps a closed column trigger focused after Escape', async () => {
    render(<LocalGrid />)
    const user = userEvent.setup()
    const trigger = within(screen.getByRole('columnheader', { name: /Name/ })).getByRole('button')
    trigger.focus()
    await user.keyboard('{Escape}')
    expect(screen.queryByRole('menu')).toBeNull()
    expect(trigger).toHaveFocus()
    await user.keyboard('{Enter}')
    expect(screen.getByRole('menu')).toBeVisible()
    await user.keyboard('{Escape}')
    expect(screen.queryByRole('menu')).toBeNull()
    expect(trigger).toHaveFocus()
  })
})
