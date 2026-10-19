import { expect, test, type Locator } from '@playwright/test'
import iamTokens from '../../src/shared/api/mocks/fixtures/iam-tokens'
import { openMockedApp, watchBrowserIssues } from './mock-app'

const tokens = iamTokens()

async function expectInsideViewport(element: Locator, width: number) {
  const box = await element.boundingBox()
  expect(box).not.toBeNull()
  expect(box!.x).toBeGreaterThanOrEqual(0)
  expect(box!.x + box!.width).toBeLessThanOrEqual(width)
}

for (const theme of ['light', 'dark']) {
  for (const phase of ['loading', 'error', 'empty', 'no-match']) {
    test(`keeps ${phase} grid state visible at 390px in ${theme}`, async ({ page }, testInfo) => {
      await page.setViewportSize({ width: 390, height: 844 })
      await page.addInitScript(
        ({ theme, phase }) => {
          localStorage.setItem('ironrag.v1.theme', JSON.stringify(theme))
          const fetch = window.fetch.bind(window)
          window.fetch = (input, init) => {
            const url = input instanceof Request ? input.url : String(input)
            if (new URL(url, location.origin).pathname === '/v1/iam/tokens') {
              if (phase === 'loading') return new Promise<Response>(() => {})
              if (phase === 'error') return Promise.reject(new TypeError('Network unavailable'))
            }
            return fetch(input, init)
          }
        },
        { theme, phase },
      )
      await openMockedApp(page, '/admin/access', {
        authenticated: true,
        tokens: phase === 'empty' ? [] : tokens,
      })
      if (phase === 'no-match') await page.getByRole('searchbox').fill('no-matching-token')
      const boundary = page.locator('.data-grid .state-card')
      await expect(boundary).toBeVisible({ timeout: 15_000 })
      await expectInsideViewport(boundary, 390)
      await expectInsideViewport(boundary.locator('strong'), 390)
      if (phase === 'error') {
        const retry = boundary.getByRole('button', { name: /Try again/i })
        await expectInsideViewport(retry, 390)
        await retry.focus()
        await expect(retry).toBeFocused()
      }
      if (phase === 'error' || phase === 'loading') {
        await expect(page.locator('.data-grid__pagination')).toHaveCount(0)
      }
      await expect(page.getByRole('columnheader').first()).toBeVisible()
      expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBe(390)
      await page.screenshot({ path: testInfo.outputPath(`${phase}-${theme}-390.png`) })
    })
  }
}

for (const [viewport, width] of [
  ['desktop', 1280],
  ['mobile', 390],
] as const) {
  test(`tokens use column menus and whole-set search on ${viewport}`, async ({
    page,
  }, testInfo) => {
    const issues = watchBrowserIssues(page)
    await page.setViewportSize({ width, height: 844 })
    await openMockedApp(page, '/admin/access', { authenticated: true, tokens })
    const table = page.getByRole('table')
    await expect(table).toBeVisible()
    expect(
      await page.evaluate(() => ({
        width: innerWidth,
        scroll: document.documentElement.scrollWidth,
      })),
      'initial layout',
    ).toEqual({ width, scroll: width })
    const search = page.getByRole('searchbox')
    await expect(search).toHaveCount(1)

    // This value belongs to a row beyond the first 25, not to a loaded page.
    await search.fill('Needle token')
    await expect(table.getByText('Needle token', { exact: true })).toBeVisible()
    await expect(table.getByText('Row token 00', { exact: true })).toHaveCount(0)
    // Search includes the displayed non-name date column as well.
    await search.fill('2037')
    await expect(table.getByText('Needle token', { exact: true })).toBeVisible()
    await search.fill('')

    const header = table.getByRole('columnheader').first()
    await header.getByRole('button').click()
    await page.getByRole('menuitem', { name: /descending/i }).click()
    await expect(header).toHaveAttribute('aria-sort', 'descending')
    await expect(table.getByRole('row').nth(1)).toContainText('Row token 29')

    const status = table.getByRole('columnheader', { name: /Status/i })
    await status.getByRole('button').click()
    const valueSearch = page.getByRole('searchbox', { name: 'Search values' })
    await valueSearch.click()
    await valueSearch.pressSequentially('active')
    await expect(valueSearch).toBeFocused()
    await expect(page.getByRole('menuitemcheckbox')).toHaveCount(1)
    await page.getByRole('menuitemcheckbox', { name: /Active/i }).click()
    await page.keyboard.press('Escape')
    await search.fill('revoked')
    await expect(table.getByText('Row token 01', { exact: true })).toHaveCount(0)
    await search.fill('')
    await page.getByRole('button', { name: /Remove filter.*Status.*Active/i }).click()
    await search.fill('revoked')
    await expect(table).toContainText('revoked')
    await search.fill('')

    await page.screenshot({ path: testInfo.outputPath(`tokens-${viewport}.png`), fullPage: true })
    expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBe(width)
    await issues.assertClean()
  })
}

test('a column menu is keyboard reachable and Escape returns focus to its header', async ({
  page,
}) => {
  await openMockedApp(page, '/admin/access', { authenticated: true, tokens })
  const table = page.getByRole('table')
  const header = table.getByRole('columnheader').first().getByRole('button')
  await header.focus()
  await page.keyboard.press('Enter')
  await expect(page.getByRole('menu')).toBeVisible()
  await page.keyboard.press('Escape')
  await expect(header).toBeFocused()
})
