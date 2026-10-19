import { expect, test } from '@playwright/test'

import opsLibraryDashboard from '../../src/shared/api/mocks/fixtures/ops-library-dashboard'
import { openMockedApp, watchBrowserIssues } from './mock-app'

// Independent page fixtures must not share one browser deadline or mock state.
for (const locale of ['en', 'ru']) {
  for (const theme of ['light', 'dark']) {
    for (const width of [1440, 390]) {
      for (const bucket of ['vectorgap', 'graphgap', 'graphready'] as const) {
        for (const empty of [true, false]) {
          test(`indexing notices keep explanation and action readable for ${bucket} ${empty ? 'empty' : 'populated'} in ${locale} ${theme} at ${width}px`, async ({
            page,
          }, testInfo) => {
            const issues = watchBrowserIssues(page)
            const writes: string[] = []
            page.on('request', (request) => {
              if (
                new URL(request.url()).pathname.startsWith('/v1/') &&
                request.method() !== 'GET'
              ) {
                writes.push(request.method())
              }
            })
            await page.setViewportSize({ width, height: 844 })
            // The stored choice must win over the system colour scheme.
            await page.emulateMedia({ colorScheme: theme === 'dark' ? 'light' : 'dark' })
            await page.addInitScript(
              ({ locale, theme }) => {
                localStorage.setItem('ironrag.v1.locale', locale)
                localStorage.setItem('ironrag.v1.theme', JSON.stringify(theme))
              },
              { locale, theme },
            )
            await openMockedApp(page, `/documents?status=${bucket}`, {
              authenticated: true,
              documents: {
                ...(empty ? { items: [] } : {}),
                document: {
                  id: 'notice-document',
                  fileName: 'notice.md',
                  body: 'notice fixture',
                  status: 'ready',
                  readiness: bucket === 'graphgap' ? 'graph_sparse' : 'graph_ready',
                  vectorGap: bucket === 'vectorgap',
                },
                revisions: [],
              },
            })
            await expect(page.locator('html')).toHaveAttribute('data-theme', theme)
            const notice = page.getByRole('note')
            await expect(notice).toBeVisible()
            await expect(page.locator('tbody tr[data-row-id]')).toHaveCount(empty ? 0 : 1)
            const body = notice.locator('.documents-filter-notice__body')
            await expect(body).toBeVisible()
            const bounds = await body.boundingBox()
            expect(bounds).not.toBeNull()
            expect(bounds!.width).toBeGreaterThan(180)
            const noticeBounds = await notice.boundingBox()
            expect(noticeBounds!.height).toBeLessThan(500)
            for (const part of await notice.locator('strong, p, small, button').all()) {
              const box = await part.boundingBox()
              expect(box!.x).toBeGreaterThanOrEqual(0)
              expect(box!.x + box!.width).toBeLessThanOrEqual(width)
            }
            if (!empty && bucket !== 'graphready') {
              const select = notice.getByRole('button')
              await select.focus()
              await page.keyboard.press('Enter')
              await expect(page.locator('.bulk-bar')).toBeVisible()
            } else {
              await expect(notice.getByRole('button')).toHaveCount(0)
            }
            expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBe(width)
            await notice.screenshot({
              path: testInfo.outputPath(`${bucket}-${empty ? 'empty' : 'select'}.png`),
            })
            if (bucket === 'vectorgap' && empty) {
              await page.screenshot({ path: testInfo.outputPath('notice-viewport.png') })
            }
            expect(writes).toEqual([])
            await issues.assertClean()
          })
        }
      }
    }
  }
}

test('renders 63% readiness for 43 of 68 documents even with complete relationships', async ({
  page,
}, testInfo) => {
  const issues = watchBrowserIssues(page)
  const fixture = opsLibraryDashboard()
  const dashboard = opsLibraryDashboard({
    documentMetrics: {
      ...fixture.documentMetrics,
      failed: 0,
      graphReady: 43,
      graphSparse: 0,
      processing: 8,
      queued: 17,
      ready: 43,
      total: 68,
    },
    attention: [],
    recentDocuments: [],
    recentWebRuns: [],
  })

  await openMockedApp(page, '/dashboard', { authenticated: true, dashboard })

  const donut = page.getByRole('progressbar', {
    name: 'Share of all library documents ready for answers',
  })
  await expect(donut).toBeVisible()
  await expect(donut).toHaveAttribute('aria-valuemin', '0')
  await expect(donut).toHaveAttribute('aria-valuemax', '100')
  await expect(donut).toHaveAttribute('aria-valuenow', '63')
  await expect(donut).toHaveAttribute('aria-valuetext', 'Documents ready for answers: 43 of 68')
  await expect(donut).toContainText('63%')
  await expect(page.locator('.coverage-panel .badge')).toHaveText(
    'Ready documents have relationships',
  )
  await expect(
    page.getByRole('progressbar', { name: 'With fact relationships', exact: true }),
  ).toHaveAttribute('aria-valuenow', '63')

  const visual = await donut.evaluate((element) => {
    const inline = element as HTMLElement
    return {
      coverage: inline.style.getPropertyValue('--coverage-percent'),
      backgroundImage: getComputedStyle(element).backgroundImage,
    }
  })
  expect(visual.coverage).toBe('63%')
  expect(visual.backgroundImage).toContain('conic-gradient')
  const stops = visual.backgroundImage.match(
    /^conic-gradient\((.+?) 0deg, \1 63%, (.+?) 63%, \2 100%\)$/u,
  )
  expect(stops).not.toBeNull()
  expect(stops?.[1]).not.toBe(stops?.[2])
  await page
    .locator('.coverage-panel')
    .screenshot({ path: testInfo.outputPath('readiness-desktop.png') })
  await page.setViewportSize({ width: 390, height: 844 })
  await expect(donut).toBeVisible()
  await expect(page.locator('.coverage-panel .badge')).toBeVisible()
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
  await page
    .locator('.coverage-panel')
    .screenshot({ path: testInfo.outputPath('readiness-mobile.png') })
  await page.evaluate(() => {
    localStorage.setItem('ironrag.v1.locale', 'ru')
    localStorage.setItem('ironrag.v1.theme', JSON.stringify('dark'))
  })
  await page.emulateMedia({ colorScheme: 'dark' })
  await page.reload()
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark')
  const russianRing = page.getByRole('progressbar', {
    name: 'Доля готовых к ответам документов во всей библиотеке',
  })
  await expect(russianRing).toHaveAttribute('aria-valuenow', '63')
  await expect(russianRing).toHaveAttribute('aria-valuetext', 'Готовых документов: 43 из 68')
  await expect(page.locator('.coverage-panel .badge')).toHaveText('Связи у готовых документов')
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
  await page
    .locator('.coverage-panel')
    .screenshot({ path: testInfo.outputPath('readiness-mobile-ru-dark.png') })
  await page.setViewportSize({ width: 1440, height: 900 })
  await page
    .locator('.coverage-panel')
    .screenshot({ path: testInfo.outputPath('readiness-desktop-ru-dark.png') })
  await issues.assertClean()
})

test('explains memory readiness in everyday language and keeps technical terms optional', async ({
  page,
}) => {
  const issues = watchBrowserIssues(page)

  await openMockedApp(page, '/dashboard', { authenticated: true })

  const readinessPanel = page.locator('.coverage-panel')
  const statusPanel = page.locator('.memory-status-panel')

  // The sidebar entry carries the same wording now, so this has to name the
  // header specifically rather than match "Dashboard" anywhere on screen.
  await expect(page.locator('.page-header .eyebrow')).toHaveText('Dashboard')
  await expect(page.getByRole('heading', { name: 'Document readiness' })).toBeVisible()
  await expect(readinessPanel.getByRole('link', { name: /^Ready for answers/ })).toBeVisible()
  await expect(
    readinessPanel.getByText('Without fact relationships', { exact: true }),
  ).toBeVisible()
  await expect(page.getByRole('heading', { name: 'Notices' })).toBeVisible()
  await expect(statusPanel.getByRole('heading', { name: 'Need attention' })).toBeVisible()
  await expect(
    statusPanel.getByText('Some documents were not added', { exact: true }),
  ).toBeVisible()

  // Several notices can be raised at once from independent sources, and only
  // identical codes are de-duplicated. Two of them reading almost the same
  // sentence leaves the reader unable to tell which one they already handled.
  const noticeTitles = await statusPanel.locator('.attention-item strong').allInnerTexts()
  expect(noticeTitles.length).toBeGreaterThan(1)
  expect(new Set(noticeTitles).size).toBe(noticeTitles.length)

  const technicalSummary = readinessPanel.getByText('Technical details', { exact: true })
  const technicalStatus = readinessPanel.getByText('Graph state', {
    exact: true,
  })
  await expect(technicalSummary).toBeVisible()
  await expect(technicalStatus).toBeHidden()

  await technicalSummary.click()
  await expect(technicalStatus).toBeVisible()
  await issues.assertClean()
})

test('boots an authenticated shell and navigates without browser or API errors', async ({
  page,
}) => {
  const issues = watchBrowserIssues(page)

  await openMockedApp(page, '/dashboard', { authenticated: true })

  const primaryNavigation = page.getByRole('navigation', { name: 'Navigation' })
  // The sidebar entry has to read the same as the screen it opens: an operator
  // who sees "Dashboard" at the top of the screen must be able to find
  // that same wording in the navigation.
  await expect(primaryNavigation.getByRole('link', { name: 'Dashboard' })).toBeVisible()
  await expect(page.locator('.page-header .eyebrow')).toHaveText('Dashboard')
  await expect(primaryNavigation.getByRole('link', { name: 'Documents' })).toBeVisible()
  await expect(primaryNavigation.getByRole('link', { name: 'Graph' })).toBeVisible()
  await expect(primaryNavigation.getByRole('link', { name: 'Assistant' })).toBeVisible()
  await expect(page.getByRole('heading', { name: 'Default Library' })).toBeVisible()

  await primaryNavigation.getByRole('link', { name: 'Assistant' }).click()
  await expect(page).toHaveURL(/\/assistant(?:\?|$)/u)
  await expect(
    page.getByRole('heading', { name: 'Start with the knowledge you already have' }),
  ).toBeVisible()
  await issues.assertClean()
})

const routes = [
  { label: /^Ready for answers/, bucket: 'ready', key: 'status', value: 'ready' },
  {
    label: /^Not ready for answers/,
    bucket: 'notready',
    key: 'status',
    value: 'processing,queued,failed,canceled,empty',
  },
  {
    label: /^With fact relationships/,
    bucket: 'graphready',
    key: 'indexing',
    value: 'graph_ready',
  },
  {
    label: /^Without fact relationships/,
    bucket: 'graphgap',
    key: 'indexing',
    value: 'graph_gap',
  },
  {
    label: /^Of the not-ready documents/,
    bucket: 'inflight',
    key: 'status',
    value: 'processing,queued',
  },
] as const
for (const route of routes) {
  test(`dashboard document drilldown preserves ${route.bucket} through navigation and the API request`, async ({
    page,
  }) => {
    const issues = watchBrowserIssues(page)
    await openMockedApp(page, '/dashboard', {
      authenticated: true,
      documents: {
        document: {
          id: 'route-document',
          fileName: 'route.md',
          body: 'Route test',
          readiness: route.bucket === 'graphgap' ? 'graph_sparse' : 'graph_ready',
          status:
            route.bucket === 'notready' || route.bucket === 'inflight' ? 'processing' : 'ready',
        },
        revisions: [],
      },
    })
    const request = page.waitForRequest((request) => {
      const url = new URL(request.url())
      return (
        request.method() === 'GET' &&
        url.pathname.endsWith('/documents') &&
        url.searchParams.get(route.key) === route.value
      )
    })
    await page.locator('.coverage-list').getByRole('link', { name: route.label }).click()
    await request
    await expect(page).toHaveURL(new RegExp(`/documents\\?status=${route.bucket}(?:&|$)`))
    await expect(page.getByRole('heading', { name: 'Documents', exact: true })).toBeVisible()
    await issues.assertClean()
  })
}

test('dashboard semantic-index drilldown preserves its scope and selection on desktop and mobile', async ({
  page,
}, testInfo) => {
  const issues = watchBrowserIssues(page)
  const vectorFixture = opsLibraryDashboard({
    attention: [
      {
        code: 'vector_index_gap',
        title: 'gap',
        detail: 'gap',
        level: 'error',
        routePath: '/documents?status=vectorgap',
      },
    ],
  })
  await openMockedApp(page, '/dashboard', {
    authenticated: true,
    dashboard: vectorFixture,
    documents: {
      document: {
        id: 'vector-gap-document',
        fileName: 'missing-semantic-index.md',
        body: 'Semantic index test',
        vectorGap: true,
      },
      revisions: [],
    },
  })
  const vectorRequest = page.waitForRequest((request) => {
    const url = new URL(request.url())
    return url.pathname.endsWith('/documents') && url.searchParams.get('indexing') === 'vector_gap'
  })
  await page.locator('.attention-item').click()
  await vectorRequest
  await expect(page).toHaveURL(/\/documents\?status=vectorgap(?:&|$)/u)
  await expect(page.getByRole('heading', { name: 'Documents', exact: true })).toBeVisible()
  await expect(page.getByRole('note')).toContainText('Documents missing their semantic index')
  await expect(page.getByRole('button', { name: 'Reprocess', exact: true })).toBeEnabled()
  await page.getByRole('button', { name: 'Select 1 to reprocess' }).click()
  await expect(page.locator('.bulk-bar')).toBeVisible()
  await page
    .locator('.documents-filter-notice')
    .screenshot({ path: testInfo.outputPath('semantic-gap-desktop.png') })
  await page.setViewportSize({ width: 390, height: 844 })
  await expect(page.getByRole('note')).toBeVisible()
  await expect(page.getByRole('button', { name: 'Reprocess', exact: true })).toBeEnabled()
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
  await page
    .locator('.documents-filter-notice')
    .screenshot({ path: testInfo.outputPath('semantic-gap-mobile.png') })
  await issues.assertClean()
})
