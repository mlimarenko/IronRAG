import { expect, test } from '@playwright/test'

import { openMockedApp } from './mock-app'

const title =
  'A long existing conversation title with enough words to wrap across several header lines, including additional context that also exceeds the desktop conversation column'
const session = {
  conversationState: 'active',
  createdAt: '2026-01-01T00:00:00Z',
  id: 'session-header',
  libraryId: 'library-demo-1',
  title,
  turnCount: 1,
  updatedAt: '2026-01-01T00:00:00Z',
  workspaceId: 'workspace-alpha',
}

for (const width of [1280, 390]) {
  test(`existing conversation title stays inside its header at ${width}px`, async ({ page }) => {
    await page.setViewportSize({ width, height: width === 390 ? 844 : 900 })
    await page.addInitScript(() => localStorage.setItem('ironrag.v1.locale', 'en'))
    await openMockedApp(page, '/assistant', {
      authenticated: true,
      querySessions: [session],
      queryConversations: {
        [session.id]: {
          session,
          messages: [
            {
              id: 'existing-answer',
              role: 'assistant',
              content: 'Existing answer remains readable.',
              timestamp: session.createdAt,
              executionId: 'execution-header',
            },
          ],
        },
      },
    })
    if (width === 390) await page.getByRole('button', { name: 'Sessions', exact: true }).click()
    await page.getByRole('button', { name: title }).click()
    const heading = page.locator('.chat-header strong')
    await expect(heading).toHaveText(title)
    await expect(heading).toHaveAttribute('title', title)
    await expect(page.getByText('Existing answer remains readable.', { exact: true })).toBeVisible()

    const geometry = await page.locator('.chat-header').evaluate((header) => {
      const bounds = header.getBoundingClientRect()
      const label = header.querySelector('strong')!
      const titleBounds = label.getBoundingClientRect()
      const actions = header.querySelector('.chat-header__actions')!.getBoundingClientRect()
      const range = document.createRange()
      range.selectNodeContents(label)
      const lines = Array.from(range.getClientRects())
      return {
        contained: titleBounds.top >= bounds.top && titleBounds.bottom < bounds.bottom,
        separated: titleBounds.right <= actions.left,
        singleLine: lines.every((line) => Math.abs(line.top - lines[0]!.top) < 1),
        ellipsis: getComputedStyle(label).textOverflow,
        truncated: label.scrollWidth > label.clientWidth,
        actionsContained: actions.right <= bounds.right && actions.bottom < bounds.bottom,
        messageBelow: header.nextElementSibling!.getBoundingClientRect().top >= bounds.bottom,
        noHorizontalOverflow: document.documentElement.scrollWidth <= window.innerWidth,
      }
    })
    expect(geometry).toEqual({
      contained: true,
      separated: true,
      singleLine: true,
      ellipsis: 'ellipsis',
      truncated: true,
      actionsContained: true,
      messageBelow: true,
      noHorizontalOverflow: true,
    })
    if (width === 390) {
      const sessions = page.getByRole('button', { name: 'Sessions', exact: true })
      await sessions.focus()
      await page.keyboard.press('Enter')
      await expect(page.getByRole('dialog')).toBeVisible()
      await expect(page.getByRole('button', { name: title })).toBeVisible()
      await page.keyboard.press('Escape')
      await expect(sessions).toBeFocused()
    }
    await page.screenshot({ path: `test-results/assistant-header-${width}.png` })
  })
}
