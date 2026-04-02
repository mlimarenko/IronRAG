import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

import { describe, expect, it } from 'vitest'

const documentsPagePath = fileURLToPath(new URL('./DocumentsPage.vue', import.meta.url))
const documentsListPath = fileURLToPath(
  new URL('../components/documents/DocumentsList.vue', import.meta.url),
)
const addLinkDialogPath = fileURLToPath(
  new URL('../components/documents/AddLinkDialog.vue', import.meta.url),
)
const webCrawlSettingsPanelPath = fileURLToPath(
  new URL('../components/documents/WebCrawlSettingsPanel.vue', import.meta.url),
)
const webRunActivityStripPath = fileURLToPath(
  new URL('../components/documents/WebIngestRunActivityStrip.vue', import.meta.url),
)
const webRunInspectorPath = fileURLToPath(
  new URL('../components/documents/WebIngestRunInspector.vue', import.meta.url),
)
const webRunPagesTablePath = fileURLToPath(
  new URL('../components/documents/WebIngestRunPagesTable.vue', import.meta.url),
)
const documentInspectorPanePath = fileURLToPath(
  new URL('../components/documents/DocumentInspectorPane.vue', import.meta.url),
)

describe('DocumentsPage layout contract', () => {
  it('keeps the documents shell minimal: header first, then one workbench with filters and list', () => {
    const source = readFileSync(documentsPagePath, 'utf8')

    const headerIndex = source.indexOf('<DocumentsWorkspaceHeader')
    const shellIndex = source.indexOf('rr-docs-page__workspace')
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
    expect(source).toContain('<WebIngestRunInspector')
    expect(listSource).toContain('rr-docs-table__status-action')
    expect(listSource).not.toContain('DocumentsPrimarySummary')
  })

  it('keeps collection diagnostics chrome out of the list wrapper', () => {
    const listSource = readFileSync(documentsListPath, 'utf8')

    expect(listSource).not.toContain('DocumentsPrimarySummary')
    expect(listSource).not.toContain('DocumentsDiagnosticsStrip')
    expect(listSource).not.toContain('DocumentsNoticeStack')
    expect(listSource).not.toContain('DocumentProgressCell')
  })

  it('wires add-link actions through the header and dialog without mixing web runs into document rows', () => {
    const source = readFileSync(documentsPagePath, 'utf8')

    expect(source).toContain('<AddLinkDialog')
    expect(source).toContain('@open-add-link="documentsStore.openAddLinkDialog"')
    expect(source).toContain('@submit="submitLinkRun"')
    expect(source).toContain(':active-web-runs="activeWebRuns"')
    expect(source).toContain(':recent-web-runs="recentWebRuns"')
    expect(source).toContain(':web-run-action-run-id="webRunActionRunId"')
    expect(source).toContain('@open-web-run="documentsStore.openWebRun"')
    expect(source).toContain('@cancel-web-run="documentsStore.cancelWebRun"')
  })

  it('surfaces missing extract_graph readiness as one workspace-level notice with a direct admin action', () => {
    const source = readFileSync(documentsPagePath, 'utf8')

    expect(source).toContain('missingGraphBinding')
    expect(source).toContain('documents.workspace.bindingNotice.title')
    expect(source).toContain('documents.workspace.bindingNotice.message')
    expect(source).toContain('documents.workspace.bindingNotice.action')
    expect(source).toContain('@click="openAdminBindings"')
  })

  it('defaults add-link submits to single_page and strips recursive settings unless explicitly selected', () => {
    const source = readFileSync(addLinkDialogPath, 'utf8')

    expect(source).toContain("const mode = ref<WebIngestMode>('single_page')")
    expect(source).toContain(
      "const isRecursiveMode = computed(() => mode.value === 'recursive_crawl')",
    )
    expect(source).toContain("const boundaryPolicy = ref<WebBoundaryPolicy>('same_host')")
    expect(source).toContain('const maxDepth = ref(3)')
    expect(source).toContain('const maxPages = ref(100)')
    expect(source).toContain('boundaryPolicy: isRecursiveMode.value ? boundaryPolicy.value : null')
    expect(source).toContain('maxDepth: isRecursiveMode.value ? maxDepth.value : null')
    expect(source).toContain('maxPages: isRecursiveMode.value ? maxPages.value : null')
    expect(source).toContain('type="radio"')
    expect(source).toContain('name="add-link-mode"')
    expect(source).toContain('value="recursive_crawl"')
    expect(source).toContain(':disabled="!props.recursiveEnabled"')
    expect(source).toContain('documents.dialogs.addLink.modeDescriptions.recursive_crawl')
    expect(source).toContain('documents.dialogs.addLink.immutableSettingsTitle')
    expect(source).toContain('documents.dialogs.addLink.notUsed')
  })

  it('keeps recursive crawl controls gated behind explicit non-default mode', () => {
    const source = readFileSync(webCrawlSettingsPanelPath, 'utf8')

    expect(source).toContain(`v-if="props.mode === 'single_page'"`)
    expect(source).toContain('v-else-if="!props.recursiveEnabled"')
    expect(source).toContain("'update:maxDepth'")
    expect(source).toContain("'update:maxPages'")
  })

  it('renders one canonical web-run activity strip instead of ad hoc chips and duplicated counters', () => {
    const filtersSource = readFileSync(documentsPagePath, 'utf8')
    const stripSource = readFileSync(webRunActivityStripPath, 'utf8')

    expect(filtersSource).toContain(':web-run-action-run-id="webRunActionRunId"')
    expect(stripSource).toContain('documents.webRuns.activity.titleActive')
    expect(stripSource).toContain('documents.webRuns.activity.pagesInFlight')
    expect(stripSource).toContain('documents.webRuns.actions.cancel')
    expect(stripSource).toContain('@click="emit(\'openRun\', run.runId)"')
    expect(stripSource).toContain('@click="emit(\'cancelRun\', run.runId)"')
  })

  it('keeps run inspector vocabulary explicit for partial completion, failure codes, and page-level reasons', () => {
    const inspectorSource = readFileSync(webRunInspectorPath, 'utf8')
    const pagesSource = readFileSync(webRunPagesTablePath, 'utf8')

    expect(inspectorSource).toContain('documents.webRuns.fields.failureCode')
    expect(inspectorSource).toContain('documents.webRuns.failureCodes.')
    expect(inspectorSource).toContain('documents.webRuns.activity.cancelRequestedAt')
    expect(pagesSource).toContain('documents.webRuns.reasons.')
    expect(pagesSource).toContain('documents.webRuns.pages.columns.reason')
    expect(pagesSource).toContain('documents.webRuns.pages.openDocument')
  })

  it('links web-page document provenance back to the originating run and candidate truth', () => {
    const pageSource = readFileSync(documentsPagePath, 'utf8')
    const inspectorSource = readFileSync(documentInspectorPanePath, 'utf8')

    expect(pageSource).toContain(':web-run-candidate="detailWebRunCandidate"')
    expect(pageSource).toContain('@open-web-run="documentsStore.openWebRun"')
    expect(inspectorSource).toContain('documents.details.webCandidateId')
    expect(inspectorSource).toContain('documents.details.webCandidateState')
    expect(inspectorSource).toContain('documents.details.openWebRun')
    expect(inspectorSource).toContain('@click="emit(\'openWebRun\', provenanceRunId)"')
  })
})
