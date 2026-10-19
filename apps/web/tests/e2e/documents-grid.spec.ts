import { expect, test, type Locator, type Page } from '@playwright/test'
import type { DocumentsMockConfig } from '../../src/shared/api/mocks/e2e'
import documentGridItems from '../../src/shared/api/mocks/fixtures/document-grid'
import { catalogLibrary } from '../../src/shared/api/mocks/fixtures/catalog-library'
import { openMockedApp, watchBrowserIssues } from './mock-app'

const items = documentGridItems()
const documents = {
  items,
  document: { id: items[0]!.id, fileName: items[0]!.fileName, body: 'grid fixture' },
  revisions: [],
}

const expenseScenarios: {
  name: string
  costSummaries?: DocumentsMockConfig['costSummaries']
  totals: Record<'en' | 'ru', [string, string]>
}[] = [
  {
    name: 'unknown',
    totals: {
      en: ['Cost unknown', 'Cost unknown'],
      ru: ['Стоимость неизвестна', 'Стоимость неизвестна'],
    },
  },
  {
    name: 'long mixed-currency',
    costSummaries: {
      library: {
        costCoverage: 'partial',
        costRollupFreshness: 'current',
        currencyCode: 'USD',
        documentCount: items.length,
        providerCallCount: 9,
        totalCost: '9007199254740993.125000',
        unpricedProviderCallCount: 3,
      },
      workspace: {
        costCoverage: 'complete',
        costRollupFreshness: 'current',
        currencyCode: 'CAD',
        documentCount: items.length,
        libraryCount: 1,
        providerCallCount: 10,
        totalCost: '123456789012345678.123456',
        unpricedProviderCallCount: 0,
      },
    },
    totals: {
      en: ['≥ USD 9,007,199,254,740,993.13', 'CAD 123,456,789,012,345,678.12'],
      ru: ['≥ 9 007 199 254 740 993,13 USD', '123 456 789 012 345 678,12 CAD'],
    },
  },
]

async function revealExpenseTotal(page: Page, strip: Locator, total: Locator) {
  const delta = await total.evaluate((element) => {
    const stripBounds = element.closest('.cost-strip')!.getBoundingClientRect()
    const bounds = element.getBoundingClientRect()
    if (bounds.right > stripBounds.right) return bounds.right - stripBounds.right
    return Math.min(0, bounds.left - stripBounds.left)
  })
  if (delta !== 0) {
    await strip.hover()
    await page.mouse.wheel(delta, 0)
  }
  await expect
    .poll(() =>
      total.evaluate((element) => {
        const stripBounds = element.closest('.cost-strip')!.getBoundingClientRect()
        const bounds = element.getBoundingClientRect()
        return bounds.left >= stripBounds.left - 1 && bounds.right <= stripBounds.right + 1
      }),
    )
    .toBe(true)
}

for (const [locale, label] of [
  ['en', 'Clear selection'],
  ['ru', 'Снять выделение'],
] as const) {
  test(`selection reset is named and keyboard-operable in ${locale}`, async ({
    page,
  }, testInfo) => {
    const issues = watchBrowserIssues(page, { readOnly: true })
    await page.addInitScript((value) => localStorage.setItem('ironrag.v1.locale', value), locale)
    await openMockedApp(page, '/documents', { authenticated: true, documents })
    const checkbox = page.getByRole('table').locator('tbody').getByRole('checkbox').first()
    for (const width of [1280, 390]) {
      await page.setViewportSize({ width, height: 844 })
      await checkbox.check()
      const bulk = page.locator('.bulk-bar')
      const clear = bulk.getByRole('button', { name: label, exact: true })
      for (const action of await bulk.getByRole('button').all()) {
        await expect(action).toHaveAccessibleName(/\S/u)
      }
      await clear.focus()
      await expect(clear).toBeFocused()
      await page.screenshot({ path: testInfo.outputPath(`selection-${locale}-${width}.png`) })
      await page.keyboard.press('Enter')
      await expect(bulk).toHaveCount(0)
      await expect(checkbox).not.toBeChecked()
    }
    await issues.assertClean()
  })

  for (const scenario of expenseScenarios) {
    test(`${scenario.name} expense totals stay exact and reachable in ${locale}`, async ({
      page,
    }, testInfo) => {
      const issues = watchBrowserIssues(page, { readOnly: true })
      await page.addInitScript((value) => localStorage.setItem('ironrag.v1.locale', value), locale)
      await openMockedApp(page, '/documents', {
        authenticated: true,
        documents: {
          ...documents,
          ...(scenario.costSummaries ? { costSummaries: scenario.costSummaries } : {}),
        },
      })
      const strip = page.locator('.cost-strip')
      const cells = strip.locator('.cost-strip__item')
      await expect(cells.locator('strong')).toHaveText(scenario.totals[locale])
      if (scenario.costSummaries) {
        await expect(cells.nth(0)).toHaveAttribute('title', /9007199254740993\.125000 USD · 3 /)
        await expect(cells.nth(1)).toHaveAttribute('title', /123456789012345678\.123456 CAD$/)
      }
      for (const width of [1280, 390]) {
        await page.setViewportSize({ width, height: 844 })
        const overflow = await strip.evaluate((element) => ({
          amount: element.scrollWidth - element.clientWidth,
          strategy: getComputedStyle(element).overflowX,
        }))
        if (overflow.amount > 0) expect(overflow.strategy).toBe('auto')
        for (const index of [0, 1]) {
          await revealExpenseTotal(page, strip, cells.nth(index).locator('strong'))
          await strip.screenshot({
            path: testInfo.outputPath(`expenses-${locale}-${width}-${index}.png`),
          })
        }
        expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBe(width)
      }
      await issues.assertClean()
    })
  }
}

for (const [viewport, width] of [
  ['desktop', 1280],
  ['mobile', 390],
] as const) {
  test(`documents preserve rapid facet selections before navigation renders on ${viewport}`, async ({
    page,
  }, testInfo) => {
    await page.setViewportSize({ width, height: 844 })
    const requests: URL[] = []
    page.on('request', (request) => {
      const url = new URL(request.url())
      if (url.pathname.endsWith('/documents')) requests.push(url)
    })
    await openMockedApp(page, '/documents?pageSize=25', { authenticated: true, documents })
    const table = page.getByRole('table')
    await expect(table).toBeVisible()
    await table.getByRole('columnheader', { name: /Type/i }).first().getByRole('button').click()
    const pdf = page.getByRole('menuitemcheckbox', { name: 'application/pdf', exact: true })
    const plain = page.getByRole('menuitemcheckbox', { name: 'text/plain', exact: true })
    await expect(pdf).toBeVisible()
    await expect(plain).toBeVisible()
    // One browser task prevents a render between the two genuine DOM click events.
    await page.getByRole('menu').evaluate((menu) => {
      for (const value of ['application/pdf', 'text/plain']) {
        const option = menu.querySelector<HTMLElement>(
          `[role="menuitemcheckbox"][aria-label="${value}"]`,
        )
        if (!option) throw new Error(`Missing facet option: ${value}`)
        option.click()
      }
    })
    await expect(pdf).toHaveAttribute('aria-checked', 'true')
    await expect(plain).toHaveAttribute('aria-checked', 'true')
    await expect
      .poll(() =>
        requests.some((url) => {
          const filters = JSON.parse(url.searchParams.get('filters') ?? '{}') as {
            fileType?: string[]
          }
          return (
            filters.fileType?.includes('application/pdf') && filters.fileType.includes('text/plain')
          )
        }),
      )
      .toBe(true)
    await page.screenshot({
      path: testInfo.outputPath(`rapid-facets-${viewport}.png`),
      fullPage: true,
    })
    await page.keyboard.press('Escape')
    const selectedUrl = page.url()
    await page.reload()
    await expect(table).toBeVisible()
    expect(page.url()).toBe(selectedUrl)
    await table.getByRole('columnheader', { name: /Type/i }).first().getByRole('button').click()
    await expect(pdf).toHaveAttribute('aria-checked', 'true')
    await expect(plain).toHaveAttribute('aria-checked', 'true')
  })
  test(`documents keep server-wide search, sort and multi-value filters on ${viewport}`, async ({
    page,
  }, testInfo) => {
    const issues = watchBrowserIssues(page, { readOnly: true })
    const requests: URL[] = []
    page.on('request', (request) => {
      const url = new URL(request.url())
      if (url.pathname.endsWith('/documents')) requests.push(url)
    })
    await page.setViewportSize({ width, height: 844 })
    await openMockedApp(page, '/documents?pageSize=25&snapshotAt=2020-01-01T00%3A00%3A00Z', {
      authenticated: true,
      documents,
      libraries: ['Default Library', 'Other Library'].map((displayName, index) =>
        catalogLibrary({
          id: index === 0 ? 'library-demo-1' : 'library-demo-2',
          displayName,
          workspaceId: 'workspace-alpha',
          slug: `library-${index}`,
        }),
      ),
    })
    const table = page.getByRole('table')
    await expect(table).toBeVisible()
    await expect(table.getByText('Needle document.pdf', { exact: true })).toHaveCount(0)
    const search = page.getByRole('searchbox').first()
    await expect(search).toBeVisible()
    await search.fill('2037')
    await expect(table.getByText('Needle document.pdf', { exact: true })).toBeVisible()
    await expect(page).toHaveURL(/snapshotAt=2026-10-01/)
    expect(
      requests.some(
        (url) => url.searchParams.get('search') === '2037' && !url.searchParams.has('snapshotAt'),
      ),
    ).toBe(true)
    expect(requests.some((url) => url.searchParams.get('search') === '2037')).toBe(true)
    for (const [term, kind] of [
      ['1.5 KB', 'fileSize'],
      ['1m 30s', 'pipelineTime'],
      ['04/02/2037', 'date'],
    ] as const) {
      await search.fill(term)
      await expect(table.getByText('Needle document.pdf', { exact: true })).toBeVisible()
      await expect(table.locator('tbody tr')).toHaveCount(1)
      await expect
        .poll(() =>
          requests.some((url) => {
            const ranges = JSON.parse(url.searchParams.get('searchRanges') ?? '{}') as Record<
              string,
              unknown
            >
            return url.searchParams.get('search') === term && Boolean(ranges[kind])
          }),
        )
        .toBe(true)
    }
    await search.fill('$99.125')
    await expect(table.getByText('Needle document.pdf', { exact: true })).toBeVisible()
    await expect(table.locator('tbody tr')).toHaveCount(1)
    await expect
      .poll(() =>
        requests.some(
          (url) =>
            url.searchParams.get('search') === '$99.125' &&
            url.searchParams.get('normalizedSearch') === '99.125',
        ),
      )
      .toBe(true)
    await search.fill('')
    await expect(table.getByText('Needle document.pdf', { exact: true })).toHaveCount(0)

    const nameHeader = table.getByRole('columnheader', { name: /Name|Document/i }).first()
    await nameHeader.getByRole('button').click()
    const menu = page.getByRole('menu')
    await expect(menu).toBeVisible()
    const menuBounds = await menu.boundingBox()
    expect(menuBounds).not.toBeNull()
    expect(menuBounds!.y).toBeGreaterThanOrEqual(8)
    expect(Math.round(menuBounds!.y + menuBounds!.height)).toBeLessThanOrEqual(
      page.viewportSize()!.height - 8,
    )
    await page.getByRole('menuitem', { name: /ascending/i }).click()
    await expect(nameHeader).toHaveAttribute('aria-sort', 'ascending')
    await expect(table.getByRole('row').nth(1)).toContainText('Needle document.pdf')
    expect(
      requests.some(
        (url) =>
          url.searchParams.get('sortBy') === 'file_name' &&
          url.searchParams.get('sortOrder') === 'asc',
      ),
    ).toBe(true)

    const typeHeader = table.getByRole('columnheader', { name: /Type/i }).first()
    await typeHeader.getByRole('button').click()
    await page.getByRole('menuitemcheckbox', { name: /application\/pdf/i }).click()
    // Choosing the first value must not close or remount the menu during refetch.
    await expect(page.getByRole('menu')).toBeVisible()
    await page.getByRole('menuitemcheckbox', { name: /text\/plain/i }).click()
    await page.keyboard.press('Escape')
    await expect
      .poll(() =>
        requests.some((url) => {
          const filters = JSON.parse(url.searchParams.get('filters') ?? '{}') as {
            fileType?: string[]
          }
          return filters.fileType?.length === 2
        }),
      )
      .toBe(true)
    await expect(page).toHaveURL(/snapshotAt=2026-10-01/)
    const restoreUrl = page.url()
    await page.reload()
    await expect(table.getByText('Needle document.pdf', { exact: true })).toBeVisible()
    expect(page.url()).toBe(restoreUrl)
    await typeHeader.getByRole('button').click()
    await expect(page.getByRole('menuitemcheckbox', { name: /application\/pdf/i })).toHaveAttribute(
      'aria-checked',
      'true',
    )
    await expect(page.getByRole('menuitemcheckbox', { name: /text\/plain/i })).toHaveAttribute(
      'aria-checked',
      'true',
    )
    await page.keyboard.press('Escape')
    await page.getByRole('button', { name: 'Additional columns', exact: true }).click()
    await expect(table.getByRole('columnheader', { name: /Cost.*sort and filter/i })).toBeVisible()
    await expect(table.getByRole('row').nth(1)).toContainText('$99.125')
    await expect(table.getByRole('row').nth(1)).toContainText('1m 30s')
    const durationHeader = table.getByRole('columnheader', {
      name: /Pipeline time.*sort and filter/i,
    })
    await durationHeader.getByRole('button').click()
    await page.getByRole('searchbox', { name: 'Search values', exact: true }).fill('1m 30s')
    await expect(page.getByRole('menuitemcheckbox', { name: '1m 30s', exact: true })).toBeVisible()
    await expect(page.getByRole('menuitemcheckbox')).toHaveCount(1)
    await page.getByRole('menuitemcheckbox', { name: '1m 30s', exact: true }).click()
    await page.keyboard.press('Escape')
    await expect(table.locator('tbody tr')).toHaveCount(1)

    // Library changes are not a page change: selections and inspector state
    // must not cross scopes, even when the new scope has matching row ids.
    await table.locator('tbody').getByRole('checkbox').first().check()
    await expect(page.locator('.bulk-bar')).toBeVisible()
    const switchLibrary = async (name: string) => {
      await page
        .getByRole('banner')
        .getByRole('button', { name: /Library$/ })
        .click()
      await page.getByRole('dialog').getByRole('option', { name, exact: true }).click()
      await expect(page.getByRole('dialog')).toHaveCount(0)
      await expect(
        page.getByRole('banner').getByRole('button', { name: new RegExp(`${name}$`) }),
      ).toBeVisible()
    }
    const pinnedClock = new URL(page.url()).searchParams.get('snapshotAt')
    await switchLibrary('Other Library')
    await expect(page.locator('.bulk-bar')).toHaveCount(0)
    expect(new URL(page.url()).searchParams.get('snapshotAt')).toBe(pinnedClock)
    await expect(table.locator('tbody').getByRole('checkbox').first()).not.toBeChecked()
    // Remove the duration pin and install an old clock without changing scope.
    const cleanUrl = new URL(page.url())
    cleanUrl.searchParams.delete('filters')
    cleanUrl.searchParams.set('snapshotAt', '2020-01-01T00:00:00Z')
    await page.goto(cleanUrl.toString())
    await expect(table).toBeVisible()
    const requestStart = requests.length
    await switchLibrary('Default Library')
    await expect(page).toHaveURL(/snapshotAt=2026-10-01/)
    await expect
      .poll(() =>
        requests
          .slice(requestStart)
          .some(
            (url) =>
              url.pathname.includes('/library-demo-1/') && !url.searchParams.has('snapshotAt'),
          ),
      )
      .toBe(true)
    expect(
      requests
        .slice(requestStart)
        .some(
          (url) =>
            url.pathname.includes('/library-demo-1/') &&
            url.searchParams.get('snapshotAt')?.startsWith('2020'),
        ),
    ).toBe(false)
    await testInfo.attach('layout-geometry', {
      body: JSON.stringify(
        await page.evaluate(() => ({
          width: innerWidth,
          height: innerHeight,
          bodyHeight: document.body.scrollHeight,
          rootHeight: document.documentElement.scrollHeight,
          pageHeight: document.querySelector('.page-content')?.clientHeight,
          pageScroll: document.querySelector('.page-content')?.scrollHeight,
        })),
      ),
      contentType: 'application/json',
    })
    await page.screenshot({
      path: testInfo.outputPath(`documents-${viewport}.png`),
      fullPage: false,
    })
    expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBe(width)
    await issues.assertClean()
  })
}
