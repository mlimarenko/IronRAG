import { render, screen, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { MemoryRouter } from 'react-router-dom'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import type { TFunction } from 'i18next'
import type { ReactNode } from 'react'

import i18n from '@/shared/i18n'
import { gridFacetKey, gridFacetValue } from '@/shared/data-grid'
import type { AdminLibraryRow } from './librariesAdapter'
import type { LibraryFacetRow, LibraryGridColumnId } from './librariesGrid'
import {
  costFacetOption,
  librariesColumnFilters,
  libraryActionsColumn,
  libraryFacetLabel,
  libraryFacetOptions,
  libraryGridColumns,
  librarySelectedFacets,
} from './librariesGrid'
import { costValueKey, decodeCostValueKey } from './libraryCost'

const t = i18n.t.bind(i18n) as unknown as TFunction

beforeEach(async () => {
  await i18n.changeLanguage('en')
})

const usd = {
  amount: '1.23',
  currencyCode: 'USD',
  coverage: 'complete' as const,
  unpricedCallCount: 0,
}
const usdPartial = {
  amount: '1.23',
  currencyCode: 'USD',
  coverage: 'partial' as const,
  unpricedCallCount: 2,
}
const eur = {
  amount: '1.23',
  currencyCode: 'EUR',
  coverage: 'complete' as const,
  unpricedCallCount: 0,
}

const row: AdminLibraryRow = {
  id: 'library-1',
  slug: 'manuals',
  displayName: 'Manuals',
  lifecycleState: 'archived',
  queuedJobCount: 3,
  includeDocumentHintInMcpAnswers: true,
  workspace: { displayName: 'Alpha', id: 'workspace-1', slug: 'alpha' },
  readiness: { ready: false, missingBindingPurposes: ['extract_text'] },
  costSummary: {
    costCoverage: 'complete',
    currencyCode: 'USD',
    documentCount: 12,
    providerCallCount: 34,
    totalCost: '1.23',
    unpricedProviderCallCount: 0,
  },
  costValue: usd,
}

const emptyRow: AdminLibraryRow = {
  ...row,
  id: 'library-2',
  slug: 'empty',
  displayName: 'Empty',
  lifecycleState: 'active',
  queuedJobCount: 0,
  readiness: { ready: true, missingBindingPurposes: [] },
  costSummary: null,
  costValue: null,
}

function renderCell(node: ReactNode) {
  return render(<MemoryRouter>{node}</MemoryRouter>)
}

function columnById(id: string, onOpen: (row: AdminLibraryRow) => void = () => {}) {
  const found = libraryGridColumns({ t, locale: 'en', onOpen }).find((column) => column.id === id)
  if (!found) throw new Error(`missing library grid column ${id}`)
  return found
}

describe('libraries grid column filters', () => {
  it('cost option keys survive the canonical grid fallback decoder and preserve a real NULL bucket', () => {
    expect(gridFacetValue(costFacetOption(usdPartial, 1, t, 'en').key)).toBe(
      costValueKey(usdPartial),
    )
    expect(gridFacetValue(costFacetOption(null, 1, t, 'en').key)).toBeNull()
  })
  it('does not reinterpret an invalid selected cost key as the NULL bucket', () => {
    expect(() => librariesColumnFilters({ cost: [gridFacetKey('malformed')] })).toThrow()
    expect(librariesColumnFilters({ cost: [gridFacetKey(null)] })?.cost).toEqual([null])
  })
  it('converts typed facet keys into typed filter values, keeping the null bucket', () => {
    const filters = librariesColumnFilters({
      library: [gridFacetKey('Manuals')],
      documents: [gridFacetKey(12), gridFacetKey(null)],
      readiness: [gridFacetKey(true)],
      cost: [gridFacetKey(costValueKey(usd))],
      queue: [],
    })
    expect(filters).toEqual({
      library: ['Manuals'],
      documents: [12, null],
      readiness: [true],
      cost: [usd],
    })
  })

  it('returns undefined when nothing is selected', () => {
    expect(librariesColumnFilters({ library: [] })).toBeUndefined()
  })

  it('maps every library column onto its typed wire filter', () => {
    expect(
      librariesColumnFilters({
        library: [gridFacetKey(null)],
        workspace: [gridFacetKey('Alpha')],
        documents: [gridFacetKey(12)],
        queue: [gridFacetKey(0)],
        calls: [gridFacetKey(34), gridFacetKey(null)],
        readiness: [gridFacetKey(true), gridFacetKey(false), gridFacetKey(null)],
        lifecycle: [gridFacetKey('archived')],
      }),
    ).toEqual({
      library: [null],
      workspace: ['Alpha'],
      documents: [12],
      queue: [0],
      calls: [34, null],
      readiness: [true, false, null],
      lifecycle: ['archived'],
    })
  })

  it('ignores a column the grid does not own', () => {
    expect(librariesColumnFilters({ actions: [gridFacetKey('open')] })).toBeUndefined()
  })

  it('rejects a cost selection that is not a canonical cost key', () => {
    expect(() => librariesColumnFilters({ cost: [gridFacetKey(2)] })).toThrow(
      'Invalid cost facet value',
    )
  })

  it('keeps amount, coverage and unpriced count distinct in compound cost keys', () => {
    const keys = new Set([costValueKey(usd), costValueKey(usdPartial), costValueKey(eur)])
    expect(keys.size).toBe(3)
    expect(decodeCostValueKey(costValueKey(usdPartial))).toEqual(usdPartial)
    expect(decodeCostValueKey('company:1.23')).toBeNull()
  })

  it('preserves cost metadata through the column filter', () => {
    const filters = librariesColumnFilters({
      cost: [
        gridFacetKey(costValueKey(usd)),
        gridFacetKey(costValueKey(eur)),
        gridFacetKey(costValueKey(usdPartial)),
      ],
    })
    expect(filters?.cost).toEqual([usd, eur, usdPartial])
  })

  it('labels selected facets the same way the menu offered them', () => {
    const selected = librarySelectedFacets({ readiness: [gridFacetKey(true)] }, t, 'en')
    expect(selected.readiness?.[0]?.label).toBe('Ready')
    expect(selected.readiness?.[0]?.count).toBe(-1)
  })

  it('labels the cost facet with the same formatter the cell uses', () => {
    const option = costFacetOption(usdPartial, 3, t, 'en')
    expect(option.label).toContain('1.23')
    expect(option.label).toContain('2')
    expect(option.key).toBe(gridFacetKey(costValueKey(usdPartial)))
    expect(costFacetOption(null, 1, t, 'en').label).toBe('Cost unknown')
  })

  it('maps typed cost facet rows and keeps the NULL bucket', () => {
    const options = libraryFacetOptions(
      'cost',
      [
        {
          count: 1,
          decimal: '1.23',
          currencyCode: 'USD',
          coverage: 'complete',
          unpricedCallCount: 0,
        },
        { count: 1, decimal: null, currencyCode: null, coverage: null, unpricedCallCount: null },
      ],
      t,
      'en',
    )
    expect(options[0]?.key).toBe(gridFacetKey(costValueKey(usd)))
    expect(options[1]?.label).toBe('Cost unknown')
  })

  it('rejects a cost facet row whose slots are not a canonical cost', () => {
    expect(() =>
      libraryFacetOptions(
        'cost',
        [{ count: 1, decimal: 'not-a-cost', currencyCode: 'USD', coverage: 'complete' }],
        t,
        'en',
      ),
    ).toThrow('Invalid cost facet value')
  })

  it('drops a scalar row from the cost facet and a non-value row from a scalar facet', () => {
    expect(libraryFacetOptions('cost', [{ value: 1, count: 2 }], t, 'en')).toEqual([])
    expect(
      libraryFacetOptions(
        'documents',
        [{ count: 1, decimal: '1.23', currencyCode: 'USD' }] as LibraryFacetRow[],
        t,
        'en',
      ),
    ).toEqual([])
  })

  it('labels numeric and lifecycle facet labels from the displayed value', () => {
    expect(libraryFacetLabel('queue', 1234, t, 'en')).toBe('1,234')
    expect(libraryFacetLabel('lifecycle', 'archived', t, 'en')).toBe('Archived')
  })

  it.each<[LibraryGridColumnId, string | number | boolean | null, string]>([
    ['documents', null, 'Not available'],
    ['calls', null, 'Not available'],
    ['library', null, '∅'],
    ['readiness', true, 'Ready'],
    ['readiness', false, 'Blocked'],
    ['documents', 1234, '1,234'],
    ['calls', 7, '7'],
    ['library', 'Manuals', 'Manuals'],
  ])('labels the %s facet value %s', (columnId, value, expected) => {
    expect(libraryFacetLabel(columnId, value, t, 'en')).toBe(expected)
  })

  it('maps scalar facet rows into typed options with their exact title', () => {
    expect(
      libraryFacetOptions(
        'documents',
        [
          { value: 12, count: 1 },
          { value: null, count: 2 },
        ],
        t,
        'en',
      ),
    ).toEqual([
      { key: gridFacetKey(12), value: 12, label: '12', title: '12', count: 1 },
      { key: gridFacetKey(null), value: null, label: 'Not available', count: 2 },
    ])
  })

  it('builds selected facets for every column and skips an empty selection', () => {
    const selected = librarySelectedFacets(
      {
        library: [],
        readiness: [gridFacetKey(true)],
        cost: [gridFacetKey(costValueKey(usd))],
      },
      t,
      'en',
    )
    expect(selected.library).toBeUndefined()
    expect(selected.readiness).toEqual([
      { key: gridFacetKey(true), value: true, label: 'Ready', count: -1 },
    ])
    expect(selected.cost?.[0]?.label).toContain('1.23')
    expect(selected.cost?.[0]?.count).toBe(-1)
  })
})

describe('library actions column', () => {
  function actions(overrides: { deleting?: boolean } = {}) {
    return libraryActionsColumn({
      t,
      onOpen: vi.fn(async () => undefined),
      onConfigureAi: vi.fn(),
      onBackup: vi.fn(),
      onRestore: vi.fn(),
      onDelete: vi.fn(async () => undefined),
      onDetails: vi.fn(),
      deleting: overrides.deleting ?? false,
      ...overrides,
    })
  }

  it('is the trailing service column and wires every row action', async () => {
    const onOpen = vi.fn(async () => undefined)
    const onConfigureAi = vi.fn()
    const onBackup = vi.fn()
    const onRestore = vi.fn()
    const onDelete = vi.fn(async () => undefined)
    const onDetails = vi.fn()
    const column = libraryActionsColumn({
      t,
      onOpen,
      onConfigureAi,
      onBackup,
      onRestore,
      onDelete,
      onDetails,
      deleting: false,
    })
    expect(column.id).toBe('actions')
    expect(column.label).toBe('Actions')
    expect(column.align).toBe('end')

    const { container } = renderCell(<>{column.cell(row)}</>)
    const user = userEvent.setup()
    await user.click(screen.getByRole('button', { name: 'Open' }))
    await user.click(screen.getByRole('button', { name: 'Configure AI' }))
    await user.click(screen.getByRole('button', { name: 'Backup' }))
    await user.click(screen.getByRole('button', { name: 'Restore' }))
    await user.click(screen.getByRole('button', { name: 'Delete' }))
    const buttons = within(container).getAllByRole('button')
    expect(buttons).toHaveLength(6)
    await user.click(buttons[5]!)

    expect(onOpen).toHaveBeenCalledWith(row)
    expect(onConfigureAi).toHaveBeenCalledWith(row)
    expect(onBackup).toHaveBeenCalledWith(row)
    expect(onRestore).toHaveBeenCalledWith(row)
    expect(onDelete).toHaveBeenCalledWith(row)
    expect(onDetails).toHaveBeenCalledWith(row)
  })

  it('disables deletion while the library is being removed', () => {
    renderCell(<>{actions({ deleting: true }).cell(row)}</>)
    expect(screen.getByRole('button', { name: 'Delete' })).toBeDisabled()
  })
})

describe('library grid columns', () => {
  it('declares the eight server-vocabulary data columns in reading order', () => {
    expect(
      libraryGridColumns({ t, locale: 'en', onOpen: () => {} }).map((column) => column.id),
    ).toEqual([
      'library',
      'workspace',
      'documents',
      'queue',
      'cost',
      'calls',
      'readiness',
      'lifecycle',
    ])
    expect(
      libraryGridColumns({ t, locale: 'en', onOpen: () => {} }).map((column) => column.label),
    ).toEqual([
      'Library',
      'Workspace',
      'Documents',
      'In queue',
      'Cost',
      'Calls',
      'Readiness',
      'Lifecycle',
    ])
  })

  it('opens the library from its name cell and reads the workspace name', async () => {
    const onOpen = vi.fn()
    const library = columnById('library', onOpen)
    expect(library.value(row)).toBe('Manuals')
    expect(library.displayText?.(row)).toBe('Manuals')
    renderCell(<>{library.cell?.(row)}</>)
    await userEvent.setup().click(screen.getByRole('button', { name: /Manuals/ }))
    expect(onOpen).toHaveBeenCalledWith(row)
    expect(screen.getByText('manuals')).toBeVisible()

    const workspace = columnById('workspace')
    expect(workspace.value(row)).toBe('Alpha')
    expect(workspace.displayText?.(row)).toBe('Alpha')
  })

  it('formats document and call counts, and states their absence', () => {
    const documents = columnById('documents')
    expect(documents.value(row)).toBe(12)
    expect(documents.displayText?.(row)).toBe('12')
    const counted = renderCell(<>{documents.cell?.(row)}</>)
    expect(within(counted.container).getByText('12')).toBeVisible()
    counted.unmount()
    expect(documents.value(emptyRow)).toBeNull()
    expect(documents.displayText?.(emptyRow)).toBe('Not available')
    const unknownDocuments = renderCell(<>{documents.cell?.(emptyRow)}</>)
    expect(within(unknownDocuments.container).getByText('Not available')).toBeVisible()
    unknownDocuments.unmount()

    const calls = columnById('calls')
    expect(calls.value(row)).toBe(34)
    expect(calls.displayText?.(row)).toBe('34')
    const callCount = renderCell(<>{calls.cell?.(row)}</>)
    expect(within(callCount.container).getByText('34')).toBeVisible()
    callCount.unmount()
    expect(calls.value(emptyRow)).toBeNull()
    expect(calls.displayText?.(emptyRow)).toBe('Not available')
    const unknownCalls = renderCell(<>{calls.cell?.(emptyRow)}</>)
    expect(within(unknownCalls.container).getByText('Not available')).toBeVisible()
  })

  it('links a nonzero queue count and leaves zero as plain text', () => {
    const queue = columnById('queue')
    expect(queue.value(row)).toBe(3)
    expect(queue.displayText?.(row)).toBe('3')

    const busy = renderCell(<>{queue.cell?.(row)}</>)
    expect(screen.getByRole('link', { name: 'Open queue for Manuals — jobs: 3' })).toHaveAttribute(
      'href',
      '/admin/queue?libraryId=library-1&status=queued',
    )
    busy.unmount()

    renderCell(<>{queue.cell?.(emptyRow)}</>)
    expect(screen.queryByRole('link')).toBeNull()
    expect(screen.getByText('0')).toBeVisible()
  })

  it('renders the cost cell with the shared formatter and names an unknown cost', () => {
    const cost = columnById('cost')
    expect(cost.value(row)).toBe('1.23')
    expect(cost.displayText?.(row)).toContain('1.23')
    renderCell(<>{cost.cell?.(row)}</>)
    expect(screen.getByText(/1\.23/)).toBeVisible()

    expect(cost.value(emptyRow)).toBeNull()
    expect(cost.displayText?.(emptyRow)).toBe('Cost unknown')
  })

  it('states readiness with its missing bindings and the lifecycle state', () => {
    const readiness = columnById('readiness')
    expect(readiness.value(row)).toBe(false)
    expect(readiness.displayText?.(row)).toBe('Blocked')
    const blocked = renderCell(<>{readiness.cell?.(row)}</>)
    expect(screen.getByText('Blocked').closest('.badge')).toHaveClass('badge--failed')
    expect(screen.getByText('Document Understanding')).toBeVisible()
    blocked.unmount()

    const capped = {
      ...row,
      readiness: {
        ready: false,
        missingBindingPurposes: ['extract_text', 'embed_chunk', 'query_answer'],
      },
    }
    const many = renderCell(<>{readiness.cell?.(capped)}</>)
    expect(within(many.container).getByText(/\+1$/)).toBeVisible()
    many.unmount()

    renderCell(<>{readiness.cell?.(emptyRow)}</>)
    expect(screen.getByText('Ready').closest('.badge')).toHaveClass('badge--ready')
    expect(screen.queryByText('Document Understanding')).toBeNull()
    expect(readiness.displayText?.(emptyRow)).toBe('Ready')

    const lifecycle = columnById('lifecycle')
    expect(lifecycle.value(row)).toBe('archived')
    expect(lifecycle.displayText?.(row)).toBe('Archived')
    const archived = renderCell(<>{lifecycle.cell?.(row)}</>)
    expect(screen.getByText('Archived').closest('.badge')).toHaveClass('badge--neutral')
    archived.unmount()
    renderCell(<>{lifecycle.cell?.(emptyRow)}</>)
    expect(screen.getByText('Active').closest('.badge')).toHaveClass('badge--ready')
    expect(lifecycle.displayText?.(emptyRow)).toBe('Active')
  })
})
