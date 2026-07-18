import { describe, expect, it } from 'vitest'

import {
  codeLanguageForSourceFormat,
  isCodeLikeSourceFormat,
  isEditorEditableSourceFormat,
  isMarkdownSourceFormat,
  isPlainTextSourceFormat,
  isRasterImageSourceFormat,
  isTableLikeSourceFormat,
  resolveEditorSurfaceMode,
} from './editorSurfaceMode'

describe('editorSurfaceMode', () => {
  it('treats spreadsheet formats as table mode', () => {
    expect(isTableLikeSourceFormat('xlsx')).toBe(true)
    expect(
      resolveEditorSurfaceMode({
        markdown: '| A | B |\n| --- | --- |\n| 1 | 2 |',
        sourceFormat: 'xlsx',
      }),
    ).toBe('table')
  })

  it('treats code formats as code mode', () => {
    expect(isCodeLikeSourceFormat('rs')).toBe(true)
    expect(codeLanguageForSourceFormat('rs')).toBe('rust')
    expect(resolveEditorSurfaceMode({ markdown: 'pub struct Node {}', sourceFormat: 'rs' })).toBe(
      'code',
    )
  })

  it('recognizes plain text source formats for lossless editor loading', () => {
    expect(isPlainTextSourceFormat('txt')).toBe(true)
    expect(isPlainTextSourceFormat('log')).toBe(true)
    expect(isPlainTextSourceFormat('xlsx')).toBe(false)
    expect(isMarkdownSourceFormat('md')).toBe(true)
    expect(isMarkdownSourceFormat('text/markdown')).toBe(true)
    expect(isPlainTextSourceFormat('md')).toBe(false)
  })

  it('falls back to markdown table heuristics', () => {
    expect(
      resolveEditorSurfaceMode({
        markdown: '| Name | Email |\n| --- | --- |\n| Alice | a@example.com |',
      }),
    ).toBe('table')
  })

  it('disables editor for pdf and image formats', () => {
    expect(isEditorEditableSourceFormat('pdf')).toBe(false)
    expect(isEditorEditableSourceFormat('png')).toBe(false)
    expect(isEditorEditableSourceFormat('application/pdf')).toBe(false)
    expect(isEditorEditableSourceFormat('image/png')).toBe(false)
    expect(isRasterImageSourceFormat('image/jpeg')).toBe(true)
    expect(isEditorEditableSourceFormat('xlsx')).toBe(true)
    expect(isEditorEditableSourceFormat('py')).toBe(true)
  })

  it('does not classify prose with parenthetical references as code', () => {
    expect(
      resolveEditorSurfaceMode({
        markdown: [
          'The report references the previous section (see Appendix A).',
          'A second paragraph keeps normal punctuation (and closes here).',
          'A final line also ends with a parenthetical reference (Table 1).',
        ].join('\n'),
        sourceFormat: 'application/pdf',
      }),
    ).toBe('prose')
  })

  it('handles long table-like lines without pathological backtracking', () => {
    expect(
      resolveEditorSurfaceMode({
        markdown: `|${'-'.repeat(100_000)}`,
      }),
    ).toBe('prose')
  })
})
