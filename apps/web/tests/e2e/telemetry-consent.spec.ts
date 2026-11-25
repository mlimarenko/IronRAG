import { expect, test, type Response } from '@playwright/test'

import { openMockedApp, watchBrowserIssues } from './mock-app'

const preferences = (method: 'GET' | 'PUT') => (response: Response) =>
  new URL(response.url()).pathname === '/v1/iam/preferences' &&
  response.request().method() === method

for (const width of [1440, 390]) {
  test(`account telemetry choice is remembered in the user menu at ${width}px`, async ({
    page,
  }) => {
    const issues = watchBrowserIssues(page)
    await page.setViewportSize({ width, height: width === 390 ? 844 : 1000 })
    await page.addInitScript(() => localStorage.clear())
    // The signed-in menu is actionable only after the account preference is
    // resolved; the choice itself must be acknowledged by the server.
    const accountLoaded = page.waitForResponse(preferences('GET'))
    await openMockedApp(page, '/dashboard', {
      authenticated: true,
      telemetryConsent: 'denied',
    })
    await accountLoaded

    await expect(page.getByTestId('analytics-consent')).toHaveCount(0)

    await page.getByRole('button', { name: 'User menu' }).click()
    const enable = page.getByRole('menuitem', { name: 'Turn on product analytics' })
    await expect(enable).toBeVisible()
    const persisted = page.waitForResponse(preferences('PUT'))
    await enable.click()
    expect((await persisted).status()).toBe(200)

    await page.getByRole('button', { name: 'User menu' }).click()
    const disable = page.getByRole('menuitem', { name: 'Turn off product analytics' })
    await expect(disable).toBeVisible()

    await issues.assertClean()
    // The acknowledgement, not a browser-local grant, keeps the menu settled.
    await expect(disable).toBeVisible()
  })
}
