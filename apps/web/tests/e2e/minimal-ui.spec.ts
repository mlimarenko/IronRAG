import { expect, test } from '@playwright/test'
import documentGridItems from '../../src/shared/api/mocks/fixtures/document-grid'

import { openMockedApp, watchBrowserIssues } from './mock-app'

function luminance(hex: string) {
  const channels = hex.match(/[a-f\d]{2}/gi)?.map((channel) => parseInt(channel, 16) / 255)
  if (!channels || channels.length !== 3) throw new Error(`Expected an RGB token: ${hex}`)
  const [red, green, blue] = channels.map((channel) =>
    channel <= 0.04045 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4,
  ) as [number, number, number]
  return red * 0.2126 + green * 0.7152 + blue * 0.0722
}

for (const theme of ['light', 'dark'] as const) {
  for (const width of [1440, 390]) {
    test(`uses readable semantic accents on a ${theme} dashboard at ${width}px`, async ({
      page,
    }) => {
      const issues = watchBrowserIssues(page)
      await page.setViewportSize({ width, height: width === 390 ? 844 : 1000 })
      await page.addInitScript((selectedTheme) => {
        localStorage.setItem('ironrag.v1.theme', JSON.stringify(selectedTheme))
      }, theme)
      await openMockedApp(page, '/dashboard', { authenticated: true })
      await expect(page.locator('.dashboard-metrics')).toBeVisible()
      await expect(page.locator('html')).toHaveAttribute('data-theme', theme)
      await expect(page.locator('.progress--ready')).toHaveCount(1)
      await expect(page.locator('.progress--processing')).toHaveCount(1)
      await expect(page.locator('.progress--sparse')).toHaveCount(1)

      const palette = await page.evaluate(() => {
        const styles = getComputedStyle(document.documentElement)
        const token = (name: string) => styles.getPropertyValue(`--${name}`).trim()
        const pairs: Array<readonly [string, string]> = [
          ['text', 'surface'],
          ['muted', 'surface'],
          ['muted', 'surface-2'],
          ['accent-foreground', 'accent'],
          ['accent', 'accent-soft'],
          ...['ready', 'processing', 'warning', 'failed', 'sparse'].map(
            (tone) => [tone, `${tone}-bg`] as const,
          ),
        ]
        return {
          accent: token('accent'),
          ready: token('ready'),
          processing: token('processing'),
          sparse: token('sparse'),
          pairs: pairs.map(([foreground, background]) => ({
            name: `${foreground} on ${background}`,
            foreground: token(foreground),
            background: token(background),
          })),
          ring: getComputedStyle(document.querySelector('.coverage-orbit')!).backgroundImage,
          overflow: document.documentElement.scrollWidth > window.innerWidth,
        }
      })
      expect(
        new Set([palette.accent, palette.ready, palette.processing, palette.sparse]).size,
      ).toBe(4)
      for (const { name, foreground, background } of palette.pairs) {
        const bright = Math.max(luminance(foreground), luminance(background))
        const dark = Math.min(luminance(foreground), luminance(background))
        expect((bright + 0.05) / (dark + 0.05), name).toBeGreaterThanOrEqual(4.5)
      }
      expect(palette.ring).toContain('conic-gradient')
      expect(palette.overflow).toBe(false)
      await page.screenshot({ path: `test-results/e2e/dashboard-${theme}-${width}.png` })
      await issues.assertClean()
    })
  }
}

for (const width of [1440, 390]) {
  test(`shares accessible tabs across Documents and AI settings at ${width}px`, async ({
    page,
  }) => {
    const issues = watchBrowserIssues(page)
    await page.setViewportSize({ width, height: 844 })
    const items = documentGridItems()
    const config = {
      authenticated: true,
      ai: { providers: [], models: [], accounts: [], bindings: [] },
      documents: {
        items,
        document: { id: items[0]!.id, fileName: items[0]!.fileName, body: 'grid fixture' },
        revisions: [],
      },
    }
    await openMockedApp(page, '/documents', config)
    const documents = page.getByRole('tab', { name: 'Documents', exact: true })
    await expect(documents).toBeVisible()
    await documents.focus()
    await page.keyboard.press('ArrowRight')
    const webImport = page.getByRole('tab', { name: 'Web Ingest', exact: true })
    await expect(webImport).toBeFocused()
    await expect(webImport).toHaveAttribute('aria-selected', 'true')
    await expect(page.getByRole('tabpanel', { name: 'Web Ingest', exact: true })).toBeVisible()
    await page.keyboard.press('Home')
    await expect(documents).toBeFocused()
    await expect(page.getByRole('tabpanel', { name: 'Documents', exact: true })).toBeVisible()
    await page.screenshot({ path: `test-results/e2e/documents-tabs-${width}.png` })

    await openMockedApp(page, '/admin/ai?scope=library&section=bindings', config)
    const bindings = page.getByRole('tab', { name: 'Bindings', exact: true })
    await expect(bindings).toBeVisible()
    await bindings.focus()
    await page.keyboard.press('ArrowRight')
    const accounts = page.getByRole('tab', { name: 'Accounts', exact: true })
    await expect(accounts).toBeFocused()
    await expect(page).toHaveURL(/[?&]section=accounts(?:&|$)/)
    await expect(page.getByRole('tabpanel', { name: 'Accounts', exact: true })).toBeVisible()
    await page.keyboard.press('End')
    const catalog = page.getByRole('tab', { name: 'Catalog', exact: true })
    await expect(catalog).toBeFocused()
    await expect(page).toHaveURL(/[?&]section=catalog(?:&|$)/)
    await expect(page.getByRole('tabpanel', { name: 'Catalog', exact: true })).toBeVisible()
    const overflow = await page.evaluate(
      () => document.documentElement.scrollWidth > window.innerWidth,
    )
    expect(overflow).toBe(false)
    await page.screenshot({ path: `test-results/e2e/ai-catalog-tabs-${width}.png` })
    await issues.assertClean()
  })
}

test('renders the compact neutral shell without removing navigation or scope controls', async ({
  page,
}) => {
  const issues = watchBrowserIssues(page)
  await page.setViewportSize({ width: 1440, height: 1000 })
  await openMockedApp(page, '/dashboard', { authenticated: true })
  await expect(page.locator('.dashboard-metrics')).toBeVisible()

  for (const route of ['/dashboard', '/documents', '/graph', '/assistant']) {
    await expect(page.locator(`.sidebar a[href="${route}"]`)).toBeVisible()
  }
  await expect(page.locator('.context-switcher')).toBeVisible()
  await expect(page.getByRole('heading', { level: 1 })).toHaveCount(1)
  const dimensions = await page.evaluate(() => ({
    sidebar: document.querySelector('.sidebar')?.getBoundingClientRect().width,
    topbar: document.querySelector('.topbar')?.getBoundingClientRect().height,
    background: getComputedStyle(document.body).backgroundColor,
  }))
  expect(dimensions.sidebar).toBe(224)
  expect(dimensions.topbar).toBe(48)
  expect(dimensions.background).toBe('rgb(247, 249, 252)')
  await issues.assertClean()
})

test('keeps primary actions legible in the dark theme', async ({ page }) => {
  const issues = watchBrowserIssues(page)
  await page.addInitScript(() => {
    localStorage.setItem('ironrag.v1.theme', JSON.stringify('dark'))
  })
  await openMockedApp(page, '/dashboard', { authenticated: true })
  await expect(page.locator('.dashboard-metrics')).toBeVisible()
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark')
  const action = page.locator('.page-header .button--primary').first()
  await expect(action).toBeVisible()
  const colors = await action.evaluate((element) => ({
    foreground: getComputedStyle(element).color,
    background: getComputedStyle(element).backgroundColor,
  }))
  expect(colors.foreground).toBe('rgb(19, 32, 58)')
  expect(colors.background).toBe('rgb(143, 178, 255)')
  await issues.assertClean()
})

test('keeps the narrow layout within its viewport and all routes reachable in the drawer', async ({
  page,
}) => {
  const issues = watchBrowserIssues(page)
  await page.setViewportSize({ width: 390, height: 844 })
  await openMockedApp(page, '/dashboard', { authenticated: true })
  await expect(page.locator('.dashboard-metrics')).toBeVisible()
  await expect(page.locator('.sidebar')).toBeHidden()
  await page.locator('.topbar .mobile-menu').click()
  await expect(page.locator('.mobile-drawer')).toHaveClass(/is-open/u)
  for (const route of ['/dashboard', '/documents', '/graph', '/assistant']) {
    await expect(page.locator(`.mobile-drawer a[href="${route}"]`)).toBeVisible()
  }
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(390)

  await page.setViewportSize({ width: 1440, height: 1000 })
  await expect(page.getByRole('dialog', { name: /Menu|Меню/u })).toHaveCount(0)
  await expect(page.locator('.sidebar a[href="/documents"]')).toBeVisible()
  await expect(page.locator('body')).not.toHaveCSS('pointer-events', 'none')
  await issues.assertClean()
})
