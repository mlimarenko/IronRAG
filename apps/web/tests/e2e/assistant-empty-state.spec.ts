import { expect, test } from '@playwright/test'

import { installBrowserMocks, mockPath } from './support/mocks'

test('renders the assistant empty state with browser MSW stubs', async ({ page }) => {
  await installBrowserMocks(page)

  await page.goto(mockPath('/assistant'))

  const main = page.getByRole('main')
  await expect(page.getByRole('link', { name: 'AI Assistant' })).toBeVisible()
  await expect(main.getByRole('heading', { name: 'Ask a question' })).toBeVisible()
  await expect(main.getByPlaceholder('Ask a question...')).toBeVisible()
})
