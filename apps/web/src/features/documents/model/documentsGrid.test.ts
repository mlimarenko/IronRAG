import { describe, expect, it } from 'vitest'

import i18n from '@/shared/i18n'
import { gridFacetKey } from '@/shared/data-grid'

import { documentsColumnFilters, documentFacetOptions, documentGridColumns } from './documentsGrid'
import { mapListItem } from './documentAdapter'
import documentGridItems from '@/shared/api/mocks/fixtures/document-grid'

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
    const columns = documentGridColumns({
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
      const column = columns.find((column) => column.id === columnId)!
      expect(column.value(row)).toBe(expected)
      if (column.displayText) expect(column.displayText(row)).toBe(label)
      expect(
        documentFacetOptions(columnId, [{ value: expected, count: 1 }], i18n.t, 'en')[0]?.label,
      ).toBe(label)
    }
  })
})
