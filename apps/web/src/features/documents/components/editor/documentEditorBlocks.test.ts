import { describe, expect, it } from 'vitest'

import type { PreparedSegmentItem } from '@/shared/api/documents'

import {
  buildEditorBlocks,
  serializeSourceTextForEditor,
  serializeEditorBlocks,
} from './documentEditorBlocks'

function prepareItem(input: {
  segment: {
    ordinal: number
    blockKind: PreparedSegmentItem['segment']['blockKind']
    headingTrail?: string[]
  }
  text: string
  normalizedText?: string
}): PreparedSegmentItem {
  return {
    normalizedText: input.normalizedText ?? input.text,
    segment: {
      blockKind: input.segment.blockKind,
      excerpt: input.text,
      headingTrail: input.segment.headingTrail ?? [],
      ordinal: input.segment.ordinal,
      revisionId: 'test-revision',
      sectionPath: [],
      segmentId: `segment-${input.segment.ordinal}`,
    },
    supportChunkIds: [],
    text: input.text,
  }
}

describe('documentEditorBlocks', () => {
  it('hydrates spreadsheet prepared segments into heading and table blocks', () => {
    const blocks = buildEditorBlocks(
      [
        prepareItem({
          segment: {
            ordinal: 0,
            blockKind: 'heading',
            headingTrail: ['Sheet1'],
          },
          text: '## Sheet1',
        }),
        prepareItem({
          segment: {
            ordinal: 1,
            blockKind: 'table',
          },
          text: '| Item | Quantity |\n| --- | --- |\n| Widget | 7 |',
        }),
      ],
      'xlsx',
    )

    expect(blocks).toEqual([
      { kind: 'heading', level: 2, text: 'Sheet1' },
      {
        kind: 'table',
        rows: [
          ['Item', 'Quantity'],
          ['Widget', '7'],
        ],
        sheetName: 'Sheet1',
      },
    ])
  })

  it('keeps sheet names for ods tables too', () => {
    const blocks = buildEditorBlocks(
      [
        prepareItem({
          segment: {
            ordinal: 0,
            blockKind: 'heading',
            headingTrail: ['Sheet1'],
          },
          text: '## Sheet1',
        }),
        prepareItem({
          segment: {
            ordinal: 1,
            blockKind: 'table',
          },
          text: '| Item | Quantity |\n| --- | --- |\n| Widget | 7 |',
        }),
      ],
      'ods',
    )

    expect(blocks[1]).toMatchObject({
      kind: 'table',
      sheetName: 'Sheet1',
    })
  })

  it('hydrates table_row segments that use semantic normalized text but raw markdown text', () => {
    const blocks = buildEditorBlocks(
      [
        prepareItem({
          segment: {
            ordinal: 0,
            blockKind: 'heading',
            headingTrail: ['people'],
          },
          text: '## people',
        }),
        prepareItem({
          segment: {
            ordinal: 1,
            blockKind: 'table',
          },
          text: '| Name | Email |\n| --- | --- |\n| Alice | alice@example.com |',
        }),
        prepareItem({
          segment: {
            ordinal: 2,
            blockKind: 'table_row',
          },
          text: '| Alice | alice@example.com |',
          normalizedText: 'Sheet: people | Row 1 | Name: Alice | Email: alice@example.com',
        }),
      ],
      'csv',
    )

    expect(blocks[1]).toEqual({
      kind: 'table',
      rows: [
        ['Name', 'Email'],
        ['Alice', 'alice@example.com'],
      ],
      sheetName: 'people',
    })
  })

  it('serializes canonical blocks back into markdown', () => {
    const markdown = serializeEditorBlocks([
      { kind: 'heading', level: 2, text: 'Sheet1' },
      { kind: 'list_item', text: 'First row changed' },
      {
        kind: 'table',
        rows: [
          ['Item', 'Quantity'],
          ['Widget', '9'],
        ],
      },
    ])

    expect(markdown).toBe(
      '## Sheet1\n\n- First row changed\n\n| Item | Quantity |\n| --- | --- |\n| Widget | 9 |',
    )
  })

  it('hydrates code-like source formats into one code block', () => {
    const blocks = buildEditorBlocks(
      [
        prepareItem({
          segment: {
            ordinal: 0,
            blockKind: 'paragraph',
          },
          text: 'use uuid::Uuid;',
        }),
        prepareItem({
          segment: {
            ordinal: 1,
            blockKind: 'paragraph',
          },
          text: 'pub struct Node {',
        }),
        prepareItem({
          segment: {
            ordinal: 2,
            blockKind: 'paragraph',
          },
          text: '  pub id: Uuid,',
        }),
        prepareItem({
          segment: {
            ordinal: 3,
            blockKind: 'paragraph',
          },
          text: '}',
        }),
      ],
      'rs',
    )

    expect(blocks).toEqual([
      {
        kind: 'code_block',
        language: 'rust',
        text: 'use uuid::Uuid;\npub struct Node {\n  pub id: Uuid,\n}',
      },
    ])
  })

  it('removes embedded-image extraction scaffolding from non-image document views', () => {
    const markdown = serializeEditorBlocks(
      buildEditorBlocks(
        [
          prepareItem({
            segment: {
              ordinal: 0,
              blockKind: 'heading',
              headingTrail: ['Schema'],
            },
            text: '## Schema',
          }),
          prepareItem({
            segment: {
              ordinal: 1,
              blockKind: 'paragraph',
            },
            text: '<!-- image -->',
          }),
          prepareItem({
            segment: {
              ordinal: 2,
              blockKind: 'quote_block',
            },
            text: '> Image OCR: garbled mixed OCR text',
          }),
          prepareItem({
            segment: {
              ordinal: 3,
              blockKind: 'paragraph',
            },
            text: '--- Embedded image 1 (775x350) ---\nraw OCR fallback',
          }),
          prepareItem({
            segment: {
              ordinal: 4,
              blockKind: 'paragraph',
            },
            text: 'Main document text.',
          }),
        ],
        'application/pdf',
      ),
    )

    expect(markdown).toBe('## Schema\n\nMain document text.')
  })

  it('keeps OCR text for raster-image source documents', () => {
    const markdown = serializeEditorBlocks(
      buildEditorBlocks(
        [
          prepareItem({
            segment: {
              ordinal: 0,
              blockKind: 'quote_block',
            },
            text: '> Image OCR: readable text from the image',
          }),
        ],
        'image/png',
      ),
    )

    expect(markdown).toBe('> Image OCR: readable text from the image')
  })

  it('collapses excessive blank lines in prose blocks', () => {
    const markdown = serializeEditorBlocks(
      buildEditorBlocks(
        [
          prepareItem({
            segment: {
              ordinal: 0,
              blockKind: 'paragraph',
            },
            text: 'First paragraph\n\n\n\nSecond paragraph\n   \n\nThird paragraph',
          }),
        ],
        'pdf',
      ),
    )

    expect(markdown).toBe('First paragraph\n\nSecond paragraph\n\nThird paragraph')
  })

  it('preserves leading tabs in code-like source formats', () => {
    const blocks = buildEditorBlocks(
      [
        prepareItem({
          segment: {
            ordinal: 0,
            blockKind: 'paragraph',
          },
          text: '\tif (user == null)',
        }),
        prepareItem({
          segment: {
            ordinal: 1,
            blockKind: 'paragraph',
          },
          text: '\t\treturn false;',
        }),
      ],
      'cs',
    )

    expect(blocks).toEqual([
      {
        kind: 'code_block',
        language: 'cs',
        text: '\tif (user == null)\n\t\treturn false;',
      },
    ])
  })

  it('serializes raw code source into one fenced block without losing leading spaces', () => {
    expect(serializeSourceTextForEditor('def run():\n    return 42\n', 'py')).toBe(
      '```python\ndef run():\n    return 42\n\n```',
    )
  })

  it('parses Markdown headings with horizontal whitespace separators', () => {
    const blocks = buildEditorBlocks([
      prepareItem({
        segment: {
          ordinal: 0,
          blockKind: 'heading',
          headingTrail: ['fallback'],
        },
        text: '##\t  Heading',
      }),
    ])

    expect(blocks).toEqual([{ kind: 'heading', level: 2, text: 'Heading' }])
  })

  it('parses very long headings and code fences without pathological backtracking', () => {
    const headingText = `#${'x'.repeat(100_000)}`
    const blocks = buildEditorBlocks([
      prepareItem({
        segment: {
          ordinal: 0,
          blockKind: 'heading',
          headingTrail: ['fallback'],
        },
        text: headingText,
      }),
      prepareItem({
        segment: {
          ordinal: 1,
          blockKind: 'code_block',
        },
        text: `\n${'x'.repeat(100_000)}`,
      }),
    ])

    expect(blocks).toEqual([
      { kind: 'heading', level: 1, text: headingText },
      { kind: 'code_block', text: 'x'.repeat(100_000), language: undefined },
    ])
  })
})
