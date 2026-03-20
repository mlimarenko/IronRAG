import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

import { describe, expect, it } from 'vitest'

const documentsPagePath = fileURLToPath(new URL('./DocumentsPage.vue', import.meta.url))
const documentsTablePath = fileURLToPath(
  new URL('../components/documents/DocumentsTable.vue', import.meta.url),
)

describe('DocumentsPage layout contract', () => {
  it('keeps the primary summary flow ahead of secondary diagnostics, notices, and the table', () => {
    const source = readFileSync(documentsPagePath, 'utf8')

    const headerIndex = source.indexOf('<DocumentsWorkspaceHeader')
    const summaryIndex = source.indexOf('<DocumentsPrimarySummary')
    const diagnosticsIndex = source.indexOf('<DocumentsDiagnosticsStrip')
    const noticesIndex = source.indexOf('<DocumentsNoticeStack')
    const filtersIndex = source.indexOf('<DocumentsFiltersBar')
    const tableIndex = source.indexOf('<DocumentsTable')

    expect(headerIndex).toBeGreaterThan(-1)
    expect(summaryIndex).toBeGreaterThan(headerIndex)
    expect(diagnosticsIndex).toBeGreaterThan(summaryIndex)
    expect(noticesIndex).toBeGreaterThan(diagnosticsIndex)
    expect(filtersIndex).toBeGreaterThan(noticesIndex)
    expect(tableIndex).toBeGreaterThan(filtersIndex)
  })

  it('keeps collection summary chrome out of the table wrapper', () => {
    const tableSource = readFileSync(documentsTablePath, 'utf8')

    expect(tableSource).not.toContain('DocumentsPrimarySummary')
    expect(tableSource).not.toContain('DocumentsDiagnosticsStrip')
    expect(tableSource).not.toContain('workspacePrimarySummary')
    expect(tableSource).not.toContain('workspaceSecondaryDiagnostics')
  })
})
