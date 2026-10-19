import { expect, test, type Page, type TestInfo } from '@playwright/test'
import type { BrowserMockConfig } from '../../src/shared/api/mocks/e2e'
import type {
  CatalogLibraryResponse,
  IngestQueueItemResponse,
} from '../../src/shared/api/generated'
import { openMockedApp, watchBrowserIssues } from './mock-app'

const libraries = ['Alpha library', 'Beta library', 'Null cost library'].map(
  (displayName, index): CatalogLibraryResponse => ({
    id: `grid-library-${index}`,
    workspaceId: `grid-workspace-${index}`,
    displayName,
    slug: `grid-library-${index}`,
    description: null,
    extractionPrompt: null,
    lifecycleState: index === 0 ? 'active' : 'archived',
    includeDocumentHintInMcpAnswers: false,
    ingestionReadiness: { ready: index === 0, missingBindingPurposes: [] },
    runtimeReadiness: { missingBindingPurposes: [] },
    recognitionPolicy: { rasterImageEngine: 'vision' },
    retrievalConfig: { lexical: { textSearchConfig: 'english' } },
    webIngestPolicy: {
      crawlFilter: { allowPatterns: [], blockPatterns: [] },
      materializationFilter: { allowPatterns: [], blockPatterns: [] },
    },
  }),
)

const items: IngestQueueItemResponse[] = []

const libraryGrid: NonNullable<BrowserMockConfig['libraryQueue']> = {
  catalog: libraries,
  items,
  libraries: {
    items: libraries.map((library, index) => ({
      ...library,
      queuedJobCount: index + 3,
      workspace: {
        id: library.workspaceId,
        slug: `grid-workspace-${index}`,
        displayName: `Grid workspace ${index}`,
      },
      costSummary:
        index === 2
          ? null
          : {
              totalCost: '1.2345',
              currencyCode: index === 0 ? 'USD' : 'EUR',
              documentCount: 1000,
              providerCallCount: 4,
              unpricedProviderCallCount: index === 0 ? 0 : 3,
              costCoverage: index === 0 ? 'complete' : 'partial',
            },
      costValue:
        index === 2
          ? null
          : {
              amount: '1.23',
              currencyCode: index === 0 ? 'USD' : 'EUR',
              coverage: index === 0 ? 'complete' : 'partial',
              unpricedCallCount: index === 0 ? 0 : 3,
            },
    })),
    totalCount: 3,
    facets: { activeCount: 1, readyCount: 1, workspaceCount: 3 },
    aggregateCostSummary: null,
  },
}

async function verifyCostFacet(page: Page, testInfo: TestInfo) {
  const label = '≥ EUR 1.23 · 3 unpriced calls'
  await page.getByRole('button', { name: 'Column Cost: sort and filter' }).click()
  const facetRequest = page.waitForRequest((request) => {
    const url = new URL(request.url())
    return (
      url.pathname === '/v1/admin/libraries/facets' && url.searchParams.get('facetSearch') === label
    )
  })
  await page.getByRole('searchbox', { name: 'Search values', exact: true }).fill(label)
  const facetQuery = new URL((await facetRequest).url()).searchParams
  expect(JSON.parse(facetQuery.get('facetSearchHints')!)).toEqual({
    costs: [{ amount: '1.23', currencyCode: 'EUR', coverage: 'partial', unpricedCallCount: 3 }],
  })
  await expect(page.getByRole('menuitemcheckbox', { name: label, exact: true })).toBeVisible()
  await expect(page.getByRole('menuitemcheckbox')).toHaveCount(1)
  await page.screenshot({ path: testInfo.outputPath('libraries-cost-facet.png'), fullPage: false })

  await page.getByRole('searchbox', { name: 'Search values', exact: true }).fill('')
  const nullOption = page.getByRole('menuitemcheckbox', { name: '∅', exact: true })
  await expect(nullOption).toBeVisible()
  // Copy the shared menu's rendered label, not the feature's cost-unknown label.
  const nullLabel = (await nullOption.getAttribute('aria-label'))!
  const nullRequest = page.waitForRequest((request) => {
    const url = new URL(request.url())
    return (
      url.pathname === '/v1/admin/libraries/facets' &&
      url.searchParams.get('facetSearch') === nullLabel
    )
  })
  await page.getByRole('searchbox', { name: 'Search values', exact: true }).fill(nullLabel)
  const nullQuery = new URL((await nullRequest).url()).searchParams
  expect(JSON.parse(nullQuery.get('facetSearchHints')!)).toEqual({ unavailable: true })
  await expect(nullOption).toBeVisible()
  await expect(page.getByRole('menuitemcheckbox')).toHaveCount(1)
  await page.screenshot({
    path: testInfo.outputPath('libraries-null-cost-facet.png'),
    fullPage: false,
  })
  await page.keyboard.press('Escape')
}

function listRequest(page: Page) {
  return page.waitForRequest(
    (request) =>
      new URL(request.url()).pathname === '/v1/admin/libraries' && request.method() === 'GET',
  )
}

test('one whole-table search reaches the server over a numeric displayed value', async ({
  page,
}, testInfo) => {
  const issues = watchBrowserIssues(page)
  await openMockedApp(page, '/admin/libraries', { authenticated: true, libraryQueue: libraryGrid })
  await expect(page.getByRole('heading', { name: 'Libraries', exact: true })).toBeVisible()

  await verifyCostFacet(page, testInfo)

  const request = listRequest(page)
  await page.getByPlaceholder('Search libraries or workspaces...').fill('3')
  const query = new URL((await request).url()).searchParams
  expect(query.get('search')).toBe('3')
  // The term names the queue column's displayed value, so the client also sends
  // the typed number hint; the server ORs it with the text search.
  expect(query.get('searchHints')).toContain('"numbers":[3]')
  await issues.assertClean()
})

test('header sort and column filter reach the server, then the queue link opens its scope', async ({
  page,
}, testInfo) => {
  const issues = watchBrowserIssues(page)
  await page.setViewportSize({ width: 390, height: 844 })
  await openMockedApp(page, '/admin/libraries', { authenticated: true, libraryQueue: libraryGrid })
  await verifyCostFacet(page, testInfo)

  const table = page.locator('table')
  await table.getByRole('button', { name: 'Column In queue: sort and filter' }).click()
  const sorted = listRequest(page)
  await page.getByRole('menuitem', { name: 'Sort descending' }).click()
  const sortQuery = new URL((await sorted).url()).searchParams
  expect(sortQuery.get('sortBy')).toBe('queue')
  expect(sortQuery.get('sortOrder')).toBe('desc')

  await page.keyboard.press('Escape')
  await table.getByRole('button', { name: 'Column Workspace: sort and filter' }).click()
  await page.getByRole('searchbox', { name: 'Search values' }).press('Control+a')
  const filtered = listRequest(page)
  await page.getByRole('menuitemcheckbox', { name: 'Grid workspace 0' }).click()
  const filterQuery = new URL((await filtered).url()).searchParams
  expect(filterQuery.get('filters')).toContain('Grid workspace 0')

  await page.keyboard.press('Escape')
  const link = table.getByRole('link', {
    name: 'Open queue for Alpha library — jobs: 3',
    exact: true,
  })
  await expect(link).toHaveAttribute('href', /libraryId=grid-library-0&status=queued/u)
  await issues.assertClean()
})
