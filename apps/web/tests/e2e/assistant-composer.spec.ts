import { expect, test, type Page } from '@playwright/test'

import { openMockedApp } from './mock-app'

const MEDIUM_TEXT = Array.from(
  { length: 4 },
  (_, index) => `Medium line ${index + 1}: short sentence that fits without internal scrolling.`,
).join('\n')
const LONG_TEXT = Array.from(
  { length: 18 },
  (_, index) =>
    `Line ${index + 1}: a wrapped sentence that keeps going for a while so the composer has to reflow instead of hiding text.`,
).join('\n')
const REFLOW_TEXT =
  'A single paragraph without hard line breaks that has to wrap differently as the conversation column narrows, which is exactly the case the field must remeasure instead of keeping its old height.'
const ELEVEN_LINE_TEXT = Array.from(
  { length: 11 },
  (_, index) => `Draft line ${index + 1}: a concise sentence that stays on one row.`,
).join('\n')
const OVERSIZED_TEXT = Array.from(
  { length: 40 },
  (_, index) =>
    `Pasted line ${index + 1}: a deliberately oversized block that must scroll inside the capped field.`,
).join('\n')

async function openAssistant(
  page: Page,
  {
    width,
    height,
    theme = 'light',
    prompt,
  }: { width: number; height: number; theme?: 'light' | 'dark'; prompt?: string },
) {
  await page.setViewportSize({ width, height })
  await page.addInitScript((selectedTheme) => {
    localStorage.setItem('ironrag.v1.locale', 'en')
    localStorage.setItem('ironrag.v1.theme', JSON.stringify(selectedTheme))
  }, theme)
  const path = prompt ? `/assistant?prompt=${encodeURIComponent(prompt)}` : '/assistant'
  await openMockedApp(page, path, { authenticated: true, telemetryConsent: 'denied' })
  const textarea = page.locator('.composer textarea')
  await expect(textarea).toBeVisible()
  return textarea
}

async function settle(page: Page) {
  await page.evaluate(() => document.fonts.ready)
  await page.evaluate(async () => {
    const transitions = document
      .getAnimations()
      .filter((animation) => animation instanceof CSSTransition)
    await Promise.all(transitions.map((animation) => animation.finished.catch(() => undefined)))
  })
  await page.evaluate(
    () =>
      new Promise<void>((resolve) =>
        requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
      ),
  )
}

async function composerMetrics(page: Page) {
  return page.locator('.composer textarea').evaluate((element) => {
    const textarea = element as HTMLTextAreaElement
    const rect = textarea.getBoundingClientRect()
    const list = document.querySelector('.message-list')!.getBoundingClientRect()
    const send = document.querySelector('.composer button')!.getBoundingClientRect()
    const content = document.querySelector('.page-content') as HTMLElement
    const style = getComputedStyle(textarea)
    return {
      height: rect.height,
      clientHeight: textarea.clientHeight,
      scrollHeight: textarea.scrollHeight,
      scrollTop: textarea.scrollTop,
      lineHeight: Number.parseFloat(style.lineHeight),
      fontSize: Number.parseFloat(style.fontSize),
      listHeight: list.height,
      sendWidth: send.width,
      sendHeight: send.height,
      sendVisible:
        send.width > 0 && send.height > 0 && send.top >= 0 && send.bottom <= window.innerHeight,
      horizontalOverflow: document.documentElement.scrollWidth > window.innerWidth,
      pageScrolls: content.scrollHeight > content.clientHeight,
      theme: document.documentElement.getAttribute('data-theme'),
    }
  })
}

test('the composer follows the draft and shrinks again after clearing', async ({ page }) => {
  const textarea = await openAssistant(page, { width: 1440, height: 900 })
  const send = page.getByRole('button', { name: 'Send', exact: true })

  const empty = await composerMetrics(page)
  expect(empty.height).toBeLessThanOrEqual(48)
  expect(empty.scrollHeight).toBeLessThanOrEqual(empty.clientHeight + 1)
  await expect(send).toBeDisabled()

  // Eleven rows fit on a tall desktop: the old 170px ceiling is gone and
  // nothing scrolls while the chat still has room.
  await textarea.fill(ELEVEN_LINE_TEXT)
  const tall = await composerMetrics(page)
  expect(tall.height).toBeGreaterThan(170)
  expect(tall.height).toBeGreaterThanOrEqual(10 * tall.lineHeight)
  expect(tall.height).toBeLessThanOrEqual(14 * tall.lineHeight + 1)
  expect(tall.height).toBeLessThanOrEqual(900 * 0.3 + 1)
  expect(tall.scrollHeight).toBeLessThanOrEqual(tall.clientHeight + 1)
  await expect(send).toBeEnabled()
  await settle(page)
  await page.screenshot({ path: 'test-results/e2e/assistant-composer-tall-draft.png' })

  // An oversized paste is capped by the text-relative and viewport limits.
  await textarea.fill(OVERSIZED_TEXT)
  const capped = await composerMetrics(page)
  expect(capped.height).toBeLessThanOrEqual(14 * capped.lineHeight + 1)
  expect(capped.height).toBeLessThanOrEqual(900 * 0.3 + 1)
  expect(capped.scrollHeight).toBeGreaterThan(capped.clientHeight)
  expect(capped.scrollTop + capped.clientHeight).toBeGreaterThanOrEqual(capped.scrollHeight - 1)
  expect(capped.sendVisible).toBe(true)
  expect(capped.horizontalOverflow).toBe(false)
  await settle(page)
  await page.screenshot({ path: 'test-results/e2e/assistant-composer-oversized.png' })

  // Typing at the cap must keep the caret's line in view instead of resetting
  // the field to the top.
  await textarea.press('Control+End')
  await page.keyboard.type('X')
  await expect
    .poll(async () => {
      const metrics = await composerMetrics(page)
      return metrics.scrollTop + metrics.clientHeight >= metrics.scrollHeight - 1
    })
    .toBe(true)

  await textarea.fill('')
  await expect.poll(async () => (await composerMetrics(page)).height).toBeLessThanOrEqual(48)
  await expect(send).toBeDisabled()
  await settle(page)
  await page.screenshot({ path: 'test-results/e2e/assistant-composer-cleared.png' })
})

test('the cap keeps history and the send button usable on a short screen', async ({ page }) => {
  const textarea = await openAssistant(page, { width: 800, height: 420 })
  await textarea.fill(LONG_TEXT)

  const metrics = await composerMetrics(page)
  expect(metrics.height).toBeLessThanOrEqual(420 * 0.3 + 1)
  expect(metrics.scrollHeight).toBeGreaterThan(metrics.clientHeight)
  expect(metrics.listHeight).toBeGreaterThan(40)
  expect(metrics.sendVisible).toBe(true)
  expect(metrics.pageScrolls).toBe(false)
  expect(metrics.horizontalOverflow).toBe(false)
  await settle(page)
  await page.screenshot({ path: 'test-results/e2e/assistant-composer-short-screen.png' })
})

for (const theme of ['light', 'dark'] as const) {
  test(`the long draft stays capped at phone width in the ${theme} theme`, async ({ page }) => {
    const textarea = await openAssistant(page, { width: 390, height: 844, theme })
    const send = page.getByRole('button', { name: 'Send', exact: true })
    await textarea.fill(LONG_TEXT)
    await expect(send).toBeEnabled()

    const metrics = await composerMetrics(page)
    expect(metrics.theme).toBe(theme)
    expect(metrics.height).toBeLessThanOrEqual(844 * 0.3 + 1)
    expect(metrics.scrollHeight).toBeGreaterThan(metrics.clientHeight)
    expect(metrics.listHeight).toBeGreaterThan(80)
    expect(metrics.sendVisible).toBe(true)
    expect(metrics.sendWidth).toBeGreaterThanOrEqual(44)
    expect(metrics.sendHeight).toBeGreaterThanOrEqual(44)
    expect(metrics.fontSize).toBeGreaterThanOrEqual(16)
    expect(metrics.horizontalOverflow).toBe(false)
    await settle(page)
    await page.screenshot({ path: `test-results/e2e/assistant-composer-phone-${theme}.png` })
  })
}

test('a programmatic prompt grows the field and a width change remeasures it', async ({ page }) => {
  const textarea = await openAssistant(page, { width: 1440, height: 900, prompt: MEDIUM_TEXT })
  const deepLink = await composerMetrics(page)
  expect(deepLink.height).toBeGreaterThan(60)

  await textarea.fill(REFLOW_TEXT)
  const wide = await composerMetrics(page)
  await page.setViewportSize({ width: 900, height: 900 })
  await expect
    .poll(async () => (await composerMetrics(page)).height)
    .toBeGreaterThan(wide.height + 20)
  const narrow = await composerMetrics(page)
  expect(narrow.height).toBeLessThanOrEqual(14 * narrow.lineHeight + 1)
  expect(narrow.horizontalOverflow).toBe(false)
})

test.describe('coarse pointer', () => {
  test.use({ hasTouch: true, viewport: { width: 1024, height: 900 } })

  test('the send target and field text grow for touch without widening the layout', async ({
    page,
  }) => {
    const textarea = await openAssistant(page, { width: 1024, height: 900 })
    await textarea.fill('Touch target check')

    const metrics = await composerMetrics(page)
    expect(metrics.sendWidth).toBeGreaterThanOrEqual(44)
    expect(metrics.sendHeight).toBeGreaterThanOrEqual(44)
    expect(metrics.fontSize).toBeGreaterThanOrEqual(16)
    expect(metrics.horizontalOverflow).toBe(false)
  })
})

test('Enter sends once and Shift+Enter adds a line', async ({ page }) => {
  const textarea = await openAssistant(page, { width: 1440, height: 900 })
  const turnRequests: string[] = []
  page.on('request', (request) => {
    if (request.method() === 'POST' && request.url().includes('/turns')) {
      turnRequests.push(request.url())
    }
  })
  await page.route('**/v1/query/sessions/*/turns', (route) =>
    route.fulfill({ status: 200, contentType: 'text/event-stream', body: '' }),
  )

  await textarea.fill('First line')
  await page.keyboard.press('Shift+Enter')
  await page.keyboard.type('Second line')
  await expect(textarea).toHaveValue('First line\nSecond line')
  expect(turnRequests).toHaveLength(0)

  await page.keyboard.press('Enter')
  await expect(textarea).toHaveValue('')
  await expect.poll(() => turnRequests.length).toBe(1)
})

test('an IME composition Enter does not send the half-typed question', async ({ page }) => {
  const textarea = await openAssistant(page, { width: 1440, height: 900 })
  const turnRequests: string[] = []
  page.on('request', (request) => {
    if (request.method() === 'POST' && request.url().includes('/turns')) {
      turnRequests.push(request.url())
    }
  })
  await page.route('**/v1/query/sessions/*/turns', (route) =>
    route.fulfill({ status: 200, contentType: 'text/event-stream', body: '' }),
  )

  await textarea.fill('Half-typed question')
  await textarea.dispatchEvent('keydown', { key: 'Enter', code: 'Enter', isComposing: true })
  await expect(textarea).toHaveValue('Half-typed question')
  expect(turnRequests).toHaveLength(0)

  // The same synthetic path without a composition must send, so the guard
  // above is proven to be the reason nothing was submitted.
  await textarea.dispatchEvent('keydown', { key: 'Enter', code: 'Enter', isComposing: false })
  await expect(textarea).toHaveValue('')
  await expect.poll(() => turnRequests.length).toBe(1)
})
