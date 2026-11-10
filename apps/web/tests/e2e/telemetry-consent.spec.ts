import { expect, test } from '@playwright/test'

import { openMockedApp, watchBrowserIssues } from './mock-app'

for (const width of [1440, 390]) {
  test(`account telemetry choice is remembered in the user menu at ${width}px`, async ({
    page,
  }) => {
    const issues = watchBrowserIssues(page)
    await page.setViewportSize({ width, height: width === 390 ? 844 : 1000 })
    await page.addInitScript(() => localStorage.clear())
    await openMockedApp(page, '/dashboard', {
      authenticated: true,
      telemetryConsent: 'denied',
    })

    await expect(page.getByTestId('analytics-consent')).toHaveCount(0)

    await page.getByRole('button', { name: 'User menu' }).click()
    const enable = page.getByRole('menuitem', { name: 'Turn on product analytics' })
    await expect(enable).toBeVisible()
    await enable.click()
    await page.getByRole('button', { name: 'User menu' }).click()
    await expect(page.getByRole('menuitem', { name: 'Turn off product analytics' })).toBeVisible()

    await issues.assertClean()
  })
}
