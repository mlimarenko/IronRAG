import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { describe, expect, it } from 'vitest'

/**
 * IRG-130 class guard: every text surface a narrow column can squeeze declares
 * how it overflows — an ellipsis on the box that owns the text, a wrap, or the
 * scroll of its parent. The rules below are the ones the audit named; a rule
 * that loses its strategy fails here even if no screen happens to clip today.
 */
function styles(file: string): string {
  return readFileSync(resolve(process.cwd(), 'src/styles', file), 'utf8')
}

/** The declaration block of an exact selector, with no nested braces in these rules. */
function ruleBody(css: string, selector: string): string {
  const escaped = selector.replace(/[.*+?^${}()|[\]\\]/gu, '\\$&')
  const match = new RegExp(`${escaped}\\s*\\{`, 'u').exec(css)
  if (!match) throw new Error(`selector not found: ${selector}`)
  const start = match.index + match[0].length - 1
  const end = css.indexOf('}', start)
  return css.slice(start + 1, end)
}

const rows: Array<{
  file: string
  selector: string
  declarations: Record<string, string | null>
}> = [
  {
    file: 'data-grid.css',
    selector: '.data-grid__scroll',
    declarations: { position: 'relative', 'overflow-x': 'auto' },
  },
  // The badge may shrink so the label below can ellipsize at all.
  {
    file: 'components.css',
    selector: '.badge',
    declarations: { 'min-width': '0', 'white-space': 'nowrap' },
  },
  // The badge label is a real child: an anonymous flex item never ellipsizes.
  {
    file: 'components.css',
    selector: '.badge__label',
    declarations: { 'min-width': '0', overflow: 'hidden', 'text-overflow': 'ellipsis' },
  },
  // `text-overflow` without `nowrap` never fires; the metric owns one line.
  {
    file: 'graph.css',
    selector: '.node-metrics strong',
    declarations: { overflow: 'hidden', 'text-overflow': 'ellipsis', 'white-space': 'nowrap' },
  },
  // The merged sibling fixes, guarded here because they had no test.
  {
    file: 'admin.css',
    selector: '.token-reveal code',
    declarations: { overflow: 'hidden', 'overflow-wrap': 'anywhere' },
  },
  {
    file: 'admin.css',
    selector: '.mcp-endpoints code',
    declarations: { overflow: 'hidden', 'text-overflow': 'ellipsis', 'white-space': 'nowrap' },
  },
  {
    file: 'admin.css',
    selector: '.client-configs summary,\n.system-prompt-header',
    declarations: { 'flex-wrap': 'wrap', 'overflow-wrap': 'anywhere' },
  },
  {
    file: 'admin.css',
    selector: '.client-configs summary .button,\n.system-prompt-header .button',
    declarations: { 'min-width': '0', 'max-width': '100%', 'white-space': 'normal' },
  },
  {
    file: 'admin.css',
    selector: '.model-picker-field__mark',
    declarations: { 'min-width': '0', overflow: 'hidden', 'text-overflow': 'ellipsis' },
  },
  {
    file: 'data-grid.css',
    selector: '.data-grid__head-label',
    declarations: { 'min-width': '0', overflow: 'hidden', 'text-overflow': 'ellipsis' },
  },
  {
    file: 'data-grid.css',
    selector: '.data-grid__filter-label',
    declarations: { 'min-width': '0', overflow: 'hidden', 'text-overflow': 'ellipsis' },
  },
  {
    file: 'data-grid.css',
    selector: '.data-grid__chip-label',
    declarations: { 'min-width': '0', overflow: 'hidden', 'text-overflow': 'ellipsis' },
  },
  {
    file: 'documents.css',
    selector: '.page-size-select',
    declarations: { 'min-width': '0' },
  },
  {
    file: 'documents.css',
    selector: '.page-size-select > span',
    declarations: { overflow: 'hidden', 'text-overflow': 'ellipsis' },
  },
  {
    file: 'graph.css',
    selector: '.graph-counts',
    declarations: { 'flex-wrap': 'wrap' },
  },
  {
    file: 'graph.css',
    selector: '.graph-layout-picker',
    declarations: { 'flex-wrap': 'wrap', 'min-width': '0' },
  },
]

describe('label overflow contract', () => {
  for (const row of rows) {
    it(`${row.file} ${row.selector}`, () => {
      const body = ruleBody(styles(row.file), row.selector)
      for (const [property, value] of Object.entries(row.declarations)) {
        const found = new RegExp(`(^|[;{\\s])${property}\\s*:\\s*([^;]+);`, 'u').exec(body)
        if (value === null) {
          expect(found?.[2]?.trim(), `${property} must stay absent`).toBeUndefined()
        } else {
          expect(found?.[2]?.trim(), property).toBe(value)
        }
      }
    })
  }
})
