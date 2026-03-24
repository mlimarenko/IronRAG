import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

import { describe, expect, it } from 'vitest'

const documentsPagePath = fileURLToPath(new URL('./DocumentsPage.vue', import.meta.url))
const documentsListPath = fileURLToPath(
  new URL('../components/documents/DocumentsList.vue', import.meta.url),
)

describe('DocumentsPage layout contract', () => {
  it('keeps the documents shell minimal: header first, then one workbench with filters and list', () => {
    const source = readFileSync(documentsPagePath, 'utf8')

    const headerIndex = source.indexOf('<DocumentsWorkspaceHeader')
    const shellIndex = source.indexOf('rr-documents__workspace-shell')
    const filtersIndex = source.indexOf('<DocumentsFiltersBar')
    const tableIndex = source.indexOf('<DocumentsList')

    expect(headerIndex).toBeGreaterThan(-1)
    expect(shellIndex).toBeGreaterThan(headerIndex)
    expect(filtersIndex).toBeGreaterThan(shellIndex)
    expect(tableIndex).toBeGreaterThan(filtersIndex)
  })

  it('keeps one inspector surface and compact inline row actions', () => {
    const source = readFileSync(documentsPagePath, 'utf8')
    const listSource = readFileSync(documentsListPath, 'utf8')

    expect(source).toContain('<DocumentInspectorPane')
    expect(listSource).toContain('rr-document-row__actions')
    expect(listSource).not.toContain('DocumentsPrimarySummary')
  })

  it('keeps collection diagnostics chrome out of the list wrapper', () => {
    const listSource = readFileSync(documentsListPath, 'utf8')

    expect(listSource).not.toContain('DocumentsPrimarySummary')
    expect(listSource).not.toContain('DocumentsDiagnosticsStrip')
    expect(listSource).not.toContain('DocumentsNoticeStack')
    expect(listSource).not.toContain('DocumentProgressCell')
  })
})
