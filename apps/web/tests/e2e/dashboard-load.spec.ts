import { expect, test } from '@playwright/test'

import { installBrowserMocks, mockPath } from './support/mocks'

test('loads the dashboard with browser MSW stubs', async ({ page }) => {
  await installBrowserMocks(page)

  await page.goto(mockPath('/'))

  await expect(page).toHaveURL(/\/dashboard$/)
  await expect(page.getByRole('heading', { name: 'Overview' })).toBeVisible()
  await expect(page.getByRole('button', { name: 'Library Default library' })).toBeVisible()
  const recentDocuments = page.locator('.workbench-surface').filter({
    has: page.getByRole('heading', { name: 'Recent Documents' }),
  })
  await expect(recentDocuments).toBeVisible()
  await expect(recentDocuments.getByText('No documents yet')).toBeVisible()
})
