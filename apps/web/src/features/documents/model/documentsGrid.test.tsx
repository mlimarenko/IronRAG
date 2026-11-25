import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import i18n from '@/shared/i18n'
import { gridFacetKey } from '@/shared/data-grid'
import type { DocumentItem } from '@/shared/types'

import {
  documentsColumnFilters,
  documentFacetLabel,
  documentFacetOptions,
  documentGridColumns,
  documentSelectedFacets,
} from './documentsGrid'
import { mapListItem } from './documentAdapter'
import documentGridItems from '@/shared/api/mocks/fixtures/document-grid'

beforeEach(async () => {
  await i18n.changeLanguage('en')
})

describe('documentsColumnFilters', () => {
  it('decodes typed facet keys into the wire filters without losing a null or a digit', () => {
    const filters = {
      status: [gridFacetKey('ready')],
      cost: [gridFacetKey('0.123456789012345678')],
      file_name: [gridFacetKey(null)],
      file_size: [gridFacetKey(1024)],
      not_a_column: [gridFacetKey('ignored')],
    }
    expect(documentsColumnFilters(filters)).toEqual({
      status: ['ready'],
      cost: ['0.123456789012345678'],
      fileName: [null],
      fileSize: [1024],
    })
  })

  it('returns undefined when nothing is selected', () => {
    expect(documentsColumnFilters({})).toBeUndefined()
  })

  it('maps the date, type and duration columns onto their own wire keys', () => {
    expect(
      documentsColumnFilters({
        uploaded_at: [gridFacetKey('2026-08-20T09:00:00.000Z')],
        finished_at: [gridFacetKey(null)],
        file_type: [gridFacetKey('application/pdf')],
        pipeline_time: [gridFacetKey(5000)],
      }),
    ).toEqual({
      uploadedAt: ['2026-08-20T09:00:00.000Z'],
      finishedAt: [null],
      fileType: ['application/pdf'],
      pipelineTime: [5000],
    })
  })

  it('treats an empty selection as no filter at all', () => {
    expect(documentsColumnFilters({ status: [] })).toBeUndefined()
  })
})

describe('documentFacetOptions', () => {
  it('derives the UI key from the typed value and keeps the exact decimal as the title', () => {
    const options = documentFacetOptions(
      'cost',
      [
        { value: '0.10', count: 2 },
        { value: null, count: 1 },
      ],
      i18n.t,
      'en',
    )
    expect(options).toEqual([
      { key: gridFacetKey('0.10'), value: '0.10', label: '$0.10', title: '0.10', count: 2 },
      { key: gridFacetKey(null), value: null, label: '', count: 1 },
    ])
  })

  it('renders a status facet through the translated enum label', async () => {
    await i18n.changeLanguage('en')
    const [option] = documentFacetOptions('status', [{ value: 'ready', count: 3 }], i18n.t, 'en')
    expect(option?.label).toBe('Ready')
    expect(option?.key).toBe(gridFacetKey('ready'))
  })
})

describe('document facet labels', () => {
  const cases: Array<[Parameters<typeof documentFacetLabel>[0], string | number, string]> = [
    ['status', 'processing', 'Processing'],
    ['file_size', 1537, '1.5 KB'],
    ['cost', '2.50', '$2.50'],
    ['pipeline_time', 115_000, '1m 55s'],
    ['pipeline_time', 45_000, '45s'],
    ['pipeline_time', Number.POSITIVE_INFINITY, '—'],
    ['file_name', 'report.pdf', 'report.pdf'],
  ]

  it.each(cases)('formats the %s facet label of %s', (columnId, value, expected) => {
    expect(documentFacetLabel(columnId, value, i18n.t, 'en')).toBe(expected)
  })

  it('formats both date facets through the shared date formatter', () => {
    for (const columnId of ['uploaded_at', 'finished_at'] as const) {
      expect(documentFacetLabel(columnId, '2026-08-20T09:00:00.000Z', i18n.t, 'en')).toContain(
        '2026',
      )
    }
  })
})

describe('documentSelectedFacets', () => {
  it('labels a selected value like the menu offered it, with no invented count', () => {
    const selected = documentSelectedFacets(
      {
        status: [gridFacetKey('ready')],
        cost: [gridFacetKey('1.20')],
        file_name: [gridFacetKey('report.pdf')],
        file_size: [gridFacetKey(null)],
      },
      i18n.t,
      'en',
    )
    expect(selected.status).toEqual([
      { key: gridFacetKey('ready'), value: 'ready', label: 'Ready', count: -1 },
    ])
    expect(selected.cost?.[0]?.label).toBe('$1.20')
    expect(selected.file_name?.[0]?.label).toBe('report.pdf')
    expect(selected.file_size).toEqual([
      { key: gridFacetKey(null), value: null, label: '', count: -1 },
    ])
  })

  it('drops a boolean selection no document column can hold instead of rendering it', () => {
    expect(documentSelectedFacets({ status: [gridFacetKey(true)] }, i18n.t, 'en')).toEqual({
      status: [{ key: gridFacetKey(true), value: null, label: '', count: -1 }],
    })
  })

  it('leaves out a column with an empty selection', () => {
    expect(documentSelectedFacets({ status: [] }, i18n.t, 'en')).toEqual({})
  })
})

const document: DocumentItem = {
  id: 'doc-1',
  documentState: 'active',
  fileName: 'Report.pdf',
  fileType: 'application/pdf',
  fileSize: 1537,
  uploadedAt: '2026-08-20T09:00:00.000Z',
  cost: '1.25',
  status: 'ready',
  readiness: 'graph_ready',
  externalKey: 'grid-document-1',
  processingStartedAt: '2026-08-20T09:00:10.000Z',
  processingFinishedAt: '2026-08-20T09:02:05.000Z',
}
const DEFAULT_CLOCK_MS = Date.parse('2026-08-20T10:00:00.000Z')

interface ColumnOptions {
  showDetailColumns?: boolean
  onOpen?: (item: DocumentItem) => void
  processingClockMs?: number
}

function columns(options: ColumnOptions = {}) {
  return documentGridColumns({
    locale: 'en',
    t: i18n.t,
    showDetailColumns: options.showDetailColumns ?? true,
    onOpen: options.onOpen ?? (() => {}),
    processingClockMs: options.processingClockMs ?? DEFAULT_CLOCK_MS,
  })
}

function column(id: string, options: ColumnOptions = {}) {
  const found = columns(options).find((candidate) => candidate.id === id)
  if (!found) throw new Error(`missing grid column ${id}`)
  return found
}

describe('document grid columns', () => {
  it('adds the detail columns only when the caller asks for them', () => {
    expect(columns().map((candidate) => candidate.id)).toEqual([
      'file_name',
      'file_type',
      'file_size',
      'uploaded_at',
      'status',
      'cost',
      'pipeline_time',
      'finished_at',
    ])
    expect(columns({ showDetailColumns: false }).map((candidate) => candidate.id)).toEqual([
      'file_name',
      'file_type',
      'file_size',
      'uploaded_at',
      'status',
    ])
  })

  it.each<[string, Partial<DocumentItem>, string]>([
    ['a web page', { sourceKind: 'web_page' }, 'lucide-globe-2'],
    ['a spreadsheet', { fileName: 'rows.csv', fileType: 'text/csv' }, 'lucide-file-spreadsheet'],
    ['a code file', { fileName: 'config.yaml', fileType: 'text/yaml' }, 'lucide-file-code-2'],
    ['any other file', {}, 'lucide-file-text'],
  ])('picks the %s source icon', (_label, overrides, iconClass) => {
    const { container } = render(
      <div>{column('file_name').cell?.({ ...document, ...overrides })}</div>,
    )
    expect(container.querySelector('svg')).toHaveClass(iconClass)
  })

  it('opens the inspector from the name cell without running the row click', async () => {
    const onOpen = vi.fn()
    const onRowClick = vi.fn()
    render(<div onClick={onRowClick}>{column('file_name', { onOpen }).cell?.(document)}</div>)
    await userEvent.setup().click(screen.getByRole('button', { name: /Report\.pdf/ }))
    expect(onOpen).toHaveBeenCalledWith(document)
    expect(onRowClick).not.toHaveBeenCalled()
    expect(screen.getByText('grid-document-1')).toBeVisible()
  })

  it('omits the external key line when it repeats the file name', () => {
    const row = { ...document, fileName: 'same.pdf', externalKey: 'same.pdf' }
    const { container } = render(<div>{column('file_name').cell?.(row)}</div>)
    expect(container.querySelector('small')).toBeNull()
  })

  it('names a document without a file name instead of rendering nothing', () => {
    const row = { ...document, fileName: null, externalKey: 'key-only' }
    render(<div>{column('file_name').cell?.(row)}</div>)
    expect(screen.getByText('Not available')).toBeVisible()
  })

  it('exposes the uploaded instant and the status as sortable, displayable values', () => {
    const uploaded = column('uploaded_at')
    expect(uploaded.value(document)).toBe('2026-08-20T09:00:00.000Z')
    expect(uploaded.displayText?.(document)).toContain('2026')

    const status = column('status')
    expect(status.value(document)).toBe('ready')
    expect(status.displayText?.(document)).toBe('Ready')
  })

  it('renders the uploaded instant with a full-date hover title', () => {
    const { container } = render(<div>{column('uploaded_at').cell?.(document)}</div>)
    const cell = container.querySelector('span')!
    expect(cell).toHaveAttribute('title', expect.stringContaining('2026'))
    expect(cell).toHaveTextContent('2026')
    expect(cell.getAttribute('title')).not.toBe(cell.textContent)
  })

  it('renders the status through the shared translated label and badge tone', () => {
    render(<div>{column('status').cell?.({ ...document, status: 'processing' })}</div>)
    expect(screen.getByText('Processing').closest('.badge')).toHaveClass('badge--processing')
  })

  it('keeps an unknown type or size visible as a dash', () => {
    expect(column('file_type').displayText?.(document)).toBe('application/pdf')
    expect(column('file_type').displayText?.({ ...document, fileType: null })).toBe('—')
    expect(column('file_size').displayText?.(document)).toBe('1.5 KB')
    expect(column('file_size').displayText?.({ ...document, fileSize: null })).toBe('—')
  })

  const costCases: Array<[string, Partial<DocumentItem>, string | null, string]> = [
    ['a known cost', { cost: '1.25' }, '1.25', '$1.25'],
    ['a reconciling rollup', { cost: null, costRollupFreshness: 'reconciling' }, null, 'Updating…'],
    ['an unpriced document', { cost: null, costRollupFreshness: 'current' }, null, '—'],
  ]

  it.each(costCases)('names the cost state for %s', (_label, overrides, rawValue, label) => {
    const costColumn = column('cost')
    expect(costColumn.value({ ...document, ...overrides })).toBe(rawValue)
    expect(costColumn.displayText?.({ ...document, ...overrides })).toBe(label)
  })

  it('measures the pipeline duration from the recorded finish', () => {
    const pipeline = column('pipeline_time')
    expect(pipeline.value(document)).toBe(115_000)
    expect(pipeline.displayText?.(document)).toBe('1m 55s')
  })

  it('measures a running pipeline against the supplied clock and dashes an unknown start', () => {
    const running: DocumentItem = {
      ...document,
      status: 'processing',
      processingFinishedAt: undefined,
    }
    const live = column('pipeline_time', {
      processingClockMs: Date.parse('2026-08-20T09:01:10.000Z'),
    })
    expect(live.value(running)).toBe(60_000)
    expect(live.displayText?.(running)).toBe('1m 0s')

    const unstarted: DocumentItem = { ...document, processingStartedAt: undefined }
    expect(column('pipeline_time').value(unstarted)).toBeNull()
    expect(column('pipeline_time').displayText?.(unstarted)).toBe('—')
  })

  it('renders the finished instant, or a dash while it has not finished', () => {
    const finished = column('finished_at')
    expect(finished.value(document)).toBe('2026-08-20T09:02:05.000Z')
    expect(finished.displayText?.(document)).toContain('2026')

    const unfinished: DocumentItem = { ...document, processingFinishedAt: undefined }
    expect(finished.value(unfinished)).toBeNull()
    expect(finished.displayText?.(unfinished)).toBe('—')
  })
})

describe('canonical row and facet values', () => {
  it('keeps MIME, encoded names, zero sizes and exact cost aligned with server keys', () => {
    const raw = {
      ...documentGridItems()[0]!,
      fileName: 'M%C3%BCller.pdf',
      fileType: 'application/pdf',
      fileSize: 0,
      cost: '9007199254740993.0000000001',
    }
    const row = mapListItem(raw, i18n.t)
    const gridColumns = documentGridColumns({
      locale: 'en',
      t: i18n.t,
      showDetailColumns: true,
      onOpen: () => {},
      processingClockMs: 0,
    })
    for (const [columnId, expected, label] of [
      ['file_name', raw.fileName, raw.fileName],
      ['file_type', raw.fileType, raw.fileType],
      ['file_size', 0, '0 B'],
      ['cost', raw.cost, `$${raw.cost}`],
    ] as const) {
      const gridColumn = gridColumns.find((candidate) => candidate.id === columnId)!
      expect(gridColumn.value(row)).toBe(expected)
      if (gridColumn.displayText) expect(gridColumn.displayText(row)).toBe(label)
      expect(
        documentFacetOptions(columnId, [{ value: expected, count: 1 }], i18n.t, 'en')[0]?.label,
      ).toBe(label)
    }
  })
})
