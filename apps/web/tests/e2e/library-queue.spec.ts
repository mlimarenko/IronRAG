import { expect, test } from '@playwright/test'
import type { BrowserMockConfig } from '../../src/shared/api/mocks/e2e'
import type { IngestQueueItemResponse } from '../../src/shared/api/generated'
import { catalogLibrary } from '../../src/shared/api/mocks/fixtures/catalog-library'
import { openMockedApp, watchBrowserIssues } from './mock-app'

const libraries = ['Target library', 'Other library', 'Empty library'].map((displayName, index) =>
  catalogLibrary({
    id: `queue-library-${index}`,
    workspaceId: `queue-workspace-${index}`,
    displayName,
    slug: `queue-library-${index}`,
  }),
)

const items = libraries.slice(0, 2).map((library, index): IngestQueueItemResponse => ({
  jobId: `queue-job-${index}`,
  jobKind: 'content_ingest',
  queueState: 'queued',
  workspaceId: library.workspaceId,
  workspaceName: `Queue workspace ${index}`,
  libraryId: library.id,
  libraryName: library.displayName,
  documentId: `queue-document-${index}`,
  documentName: `Queued document ${index}`,
  queuePosition: 1,
  queuedAt: '2026-09-30T12:00:00Z',
  availableAt: '2026-09-30T12:00:00Z',
  canCancel: true,
  canPause: true,
  canResume: false,
  canRetryRequeue: false,
  hasStaleQueueLease: false,
}))

const malformedItem: IngestQueueItemResponse = {
  availableAt: '2026-09-30T12:00:00Z',
  canCancel: true,
  canPause: true,
  canResume: false,
  canRetryRequeue: false,
  documentName: 'Malformed document',
  hasStaleQueueLease: false,
  jobId: 'queue-job-malformed',
  jobKind: 'content_ingest',
  libraryId: 'queue-library-0',
  libraryName: 'Target library',
  queueState: 'queued',
  queuedAt: '2026-09-30T12:00:00Z',
  workspaceId: 'queue-workspace-0',
  workspaceName: 'Queue workspace 0',
}

// Both payloads satisfy the generated compile-time type yet violate the wire
// contract the backend owns: a null `queueState` crashed the status label, and
// a repeated `jobId` collided React keys into duplicate rows on every sort.
const malformedQueueFixtures = [
  {
    label: 'null-queue-state',
    items: [{ ...malformedItem, queueState: null } as unknown as IngestQueueItemResponse],
    message: 'Ingest queue item 0 queueState must be a string',
  },
  {
    label: 'non-string-stage',
    items: [
      {
        ...malformedItem,
        currentStage: { stage: 'embed_chunk' },
      } as unknown as IngestQueueItemResponse,
    ],
    message: 'Ingest queue item 0 currentStage must be a string or null',
  },
  {
    label: 'duplicate-job-id',
    items: [malformedItem, { ...malformedItem, documentName: 'Duplicate row' }],
    message: 'Ingest queue response repeats jobId queue-job-malformed',
  },
] as const

// Each malformed case runs as its own bounded test with one navigation: a single
// aggregate test doing twelve navigations exhausted the 30s test budget in CI
// and left the page mid-load. Locale and width are part of the test name.
const malformedQueueLocales = [
  ['en', 'Failed to load ingest queue', 'This view could not be loaded', 'Try again'],
  ['ru', 'Не удалось загрузить очередь обработки', 'Не удалось загрузить экран', 'Повторить'],
] as const

const malformedQueueViewports = [
  ['desktop', 1440],
  ['narrow', 390],
] as const

const libraryQueue: NonNullable<BrowserMockConfig['libraryQueue']> = {
  catalog: libraries,
  items,
  libraries: {
    items: libraries.map((library, index) => ({
      ...library,
      queuedJobCount: index < 2 ? 1 : 0,
      workspace: {
        id: library.workspaceId,
        slug: `queue-workspace-${index}`,
        displayName: `Queue workspace ${index}`,
      },
      costSummary: null,
    })),
    totalCount: 3,
    facets: { activeCount: 3, readyCount: 3, workspaceCount: 3 },
    aggregateCostSummary: null,
  },
}

test('library queue count opens its exact scope and survives reload on desktop and mobile', async ({
  page,
}, testInfo) => {
  const issues = watchBrowserIssues(page, { readOnly: true })
  for (const [viewport, width] of [
    ['desktop', 1280],
    ['mobile', 390],
  ] as const) {
    await page.setViewportSize({ width, height: 844 })
    await openMockedApp(page, '/admin/libraries', { authenticated: true, libraryQueue })
    await expect(page.getByRole('heading', { name: 'Libraries', exact: true })).toBeVisible()
    await expect(
      page.locator('aside').getByRole('link', { name: 'Processing queue', exact: true }),
    ).toHaveCount(0)
    const region = page.locator('table')
    const link = region.getByRole('link', {
      name: 'Open queue for Target library — jobs: 1',
      exact: true,
    })
    await expect(link).toHaveText('1')
    const zeroLink = region.getByRole('link', {
      name: 'Open queue for Empty library — jobs: 0',
      exact: true,
    })
    await expect(zeroLink).toHaveCount(0)
    const queueColumn = await region
      .getByRole('columnheader')
      .evaluateAll((headers) =>
        headers.findIndex((header) => header.getAttribute('data-column') === 'queue'),
      )
    expect(queueColumn).toBeGreaterThanOrEqual(0)
    const zeroCell = region
      .getByRole('row')
      .filter({ hasText: 'Empty library' })
      .getByRole('cell')
      .nth(queueColumn)
    await expect(zeroCell).toHaveText('0')
    await expect(zeroCell.locator('a, button, [tabindex]')).toHaveCount(0)
    // Short values must stay readable rather than becoming character-wide
    // columns. Excess column width belongs to the same horizontal scroller.
    for (const text of ['Cost unknown', 'Queue workspace 0', 'Not available']) {
      const value = region.getByText(text, { exact: true }).first()
      await expect(value).toBeAttached()
      expect(
        await value.evaluate((element) => {
          const range = document.createRange()
          range.selectNodeContents(element)
          return new Set(Array.from(range.getClientRects(), (rect) => rect.top)).size
        }),
        `${text} remains a single readable line at ${viewport}`,
      ).toBe(1)
    }
    if (viewport === 'mobile') {
      expect(
        await region.locator('..').evaluate((element) => element.scrollWidth > element.clientWidth),
      ).toBe(true)
    }
    await region.screenshot({ path: testInfo.outputPath(`libraries-${viewport}.png`) })
    await link.scrollIntoViewIfNeeded()
    const zeroBounds = await zeroCell.boundingBox()
    expect(zeroBounds).not.toBeNull()
    expect(zeroBounds!.x).toBeGreaterThanOrEqual(0)
    expect(zeroBounds!.x + zeroBounds!.width).toBeLessThanOrEqual(width)
    expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBe(width)
    await page.screenshot({ path: testInfo.outputPath(`queued-count-${viewport}.png`) })
    const request = page.waitForRequest(
      (request) => new URL(request.url()).pathname === '/v1/ingest/queue',
    )
    await link.focus()
    await page.keyboard.press('Enter')
    const query = new URL((await request).url()).searchParams
    expect(query.get('libraryId')).toBe('queue-library-0')
    expect(query.has('workspaceId')).toBe(false)
    await expect(page).toHaveURL(/libraryId=queue-library-0&status=queued/u)
    await expect(page.getByText('Processing queue for Target library.')).toBeVisible()
    await expect(page.locator('main')).toContainText('Queued document 0')
    await expect(page.locator('main')).not.toContainText('Queued document 1')
    // The mock runtime is opt-in on each full load; navigation removes its
    // harness flag, so restore only that flag before a genuine reload.
    await page.evaluate(() => {
      const url = new URL(location.href)
      url.searchParams.set('mocks', '1')
      history.replaceState(null, '', url)
    })
    await page.reload()
    await expect(page.getByText('Processing queue for Target library.')).toBeVisible()
    await expect(page.locator('main')).not.toContainText('Queued document 1')
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    await page.locator('main').screenshot({ path: testInfo.outputPath(`queue-${viewport}.png`) })
    await page.getByRole('link', { name: 'Back to libraries' }).click()
    await expect(page.getByRole('heading', { name: 'Libraries', exact: true })).toBeVisible()
  }
  await issues.assertClean()
})

test('queue grid searches, sorts and value-filters the scoped set without a live write', async ({
  page,
}, testInfo) => {
  const issues = watchBrowserIssues(page, { readOnly: true })
  for (const [viewport, width, theme] of [
    ['desktop-light', 1440, 'light'],
    ['desktop-dark', 1440, 'dark'],
    ['mobile-light', 390, 'light'],
    ['mobile-dark', 390, 'dark'],
  ] as const) {
    await page.setViewportSize({ width, height: 844 })
    await page.addInitScript((theme) => {
      localStorage.setItem('ironrag.v1.theme', JSON.stringify(theme))
    }, theme)
    await openMockedApp(page, '/admin/queue?libraryId=queue-library-0&status=all', {
      authenticated: true,
      libraryQueue,
    })

    const table = page.getByRole('table')
    await expect(table).toBeVisible()
    await expect(page.locator('html')).toHaveAttribute('data-theme', theme)
    await expect(page).toHaveURL(/libraryId=queue-library-0/u)
    await expect(table.getByText('Queued document 0', { exact: true })).toBeVisible()

    const search = page.getByRole('searchbox').first()
    await search.fill('nothing matches this')
    await expect(page.getByText('No jobs match the filter.')).toBeVisible()
    await search.fill('')
    await expect(table.getByText('Queued document 0', { exact: true })).toBeVisible()

    // Every data header sorts.
    const position = table.getByRole('columnheader', { name: /Position/i })
    await position.getByRole('button').click()
    await page.getByRole('menuitem', { name: /descending/i }).click()
    await expect(position).toHaveAttribute('aria-sort', 'descending')
    await page.keyboard.press('Escape')

    // Every data header filters by value; the queued row survives, then the
    // empty paused bucket empties the table.
    const status = table.getByRole('columnheader', { name: /Status/i })
    await status.getByRole('button').click()
    await page.getByRole('menuitemcheckbox', { name: /Queued/i }).click()
    await page.keyboard.press('Escape')
    await expect(table.getByText('Queued document 0', { exact: true })).toBeVisible()

    // Selection is client-only; no mutation is sent by checking a row.
    await table.getByRole('checkbox').last().check()
    await expect(page.getByText(/job selected/i).first()).toBeVisible()

    await page.screenshot({
      path: testInfo.outputPath(`queue-grid-${viewport}.png`),
      fullPage: true,
    })
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    await issues.assertClean()
  }
})

for (const fixture of malformedQueueFixtures) {
  for (const [locale, fallback, heading, retryLabel] of malformedQueueLocales) {
    for (const [viewport, width] of malformedQueueViewports) {
      test(`a malformed ${fixture.label} payload shows the ${locale} error on ${viewport} (${width}px)`, async ({
        page,
      }, testInfo) => {
        const issues = watchBrowserIssues(page, { readOnly: true })
        await page.setViewportSize({ width, height: 844 })
        await page.addInitScript(
          (value) => localStorage.setItem('ironrag.v1.locale', value),
          locale,
        )
        await openMockedApp(page, '/admin/queue?libraryId=queue-library-0&status=all', {
          authenticated: true,
          libraryQueue: { ...libraryQueue, items: [...fixture.items] },
        })

        // The boundary refuses the response before the renderer sees it: the
        // localized load failure is the message, the raw field diagnostic is
        // not shown, and there is no root crash, ghost row, fake counter or
        // bulk action over rows that never loaded.
        await expect(page.getByText(fallback, { exact: true })).toBeVisible()
        await expect(page.getByText(heading, { exact: true })).toBeVisible()
        await expect(page.getByText(fixture.message, { exact: false })).toHaveCount(0)
        await expect(page.getByText('Something went wrong', { exact: false })).toHaveCount(0)
        await expect(page.locator('[data-row-id]')).toHaveCount(0)
        await expect(page.locator('.queue-summary')).toHaveCount(0)
        await expect(page.getByText(/job selected/i)).toHaveCount(0)
        await expect(page.getByRole('button', { name: retryLabel })).toBeVisible()

        await page.screenshot({
          path: testInfo.outputPath(`queue-malformed-${fixture.label}-${locale}-${viewport}.png`),
          fullPage: true,
        })
        await issues.assertClean()
      })
    }
  }
}

test('a malformed refetch gates the inspector, bulk actions and counts until the keyboard retry recovers', async ({
  page,
}, testInfo) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  await openMockedApp(page, '/admin/queue?libraryId=queue-library-0&status=all', {
    authenticated: true,
    libraryQueue,
  })

  const table = page.getByRole('table')
  await expect(table.getByText('Queued document 0', { exact: true })).toBeVisible()

  // A valid response makes the row's inspector and bulk selection available.
  await table.getByRole('checkbox').last().check()
  await expect(page.getByText(/job selected/i).first()).toBeVisible()
  await table.getByRole('button', { name: /Queued document 0/ }).click()
  const inspector = page.getByRole('dialog')
  await expect(inspector).toBeVisible()
  await expect(inspector.getByRole('button', { name: 'Top' })).toBeVisible()

  // Corrupt the served fixture in place, then force a refetch through the real
  // Refresh handler. The modal drawer covers the toolbar, so trigger the same
  // handler programmatically instead of letting the 5s poll decide timing.
  await page.evaluate(() => {
    const item = window.__IRONRAG_E2E_MOCKS__?.libraryQueue?.items?.[0]
    if (item) item.queueState = null as unknown as string
    const refresh = Array.from(document.querySelectorAll('button')).find(
      (button) => button.textContent?.trim() === 'Refresh',
    )
    refresh?.click()
  })
  await expect(page.getByText('Failed to load ingest queue', { exact: true })).toBeVisible()
  await expect(page.locator('[data-row-id]')).toHaveCount(0)
  await expect(page.getByRole('dialog')).toHaveCount(0)
  await expect(page.getByRole('button', { name: 'Top' })).toHaveCount(0)
  await expect(page.getByText(/job selected/i)).toHaveCount(0)
  await expect(page.locator('.queue-summary')).toHaveCount(0)

  await page.screenshot({
    path: testInfo.outputPath('queue-malformed-refetch-desktop.png'),
    fullPage: true,
  })

  // Repair the fixture, then recover through the keyboard alone; the row, the
  // preserved selection and the inspector all come back.
  await page.evaluate(() => {
    const item = window.__IRONRAG_E2E_MOCKS__?.libraryQueue?.items?.[0]
    if (item) item.queueState = 'queued'
  })
  const retry = page.getByRole('button', { name: 'Try again' })
  await retry.focus()
  await page.keyboard.press('Enter')

  await expect(page.locator('[data-row-id]').first()).toBeVisible()
  await expect(page.getByRole('dialog')).toBeVisible()
  await expect(page.getByRole('dialog').getByRole('button', { name: 'Top' })).toBeVisible()
  await expect(page.getByText(/job selected/i).first()).toBeVisible()
})
