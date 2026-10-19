import { expect, test, type Page } from '@playwright/test'

import { openMockedApp, watchBrowserIssues } from './mock-app'

type LabelMetrics = {
  text: string
  clientWidth: number
  scrollWidth: number
  textOverflow: string
  overflow: string
}

/**
 * Every visible sidebar label in the given scope. A label that fits its box
 * still declares the ellipsis contract, so a longer locale or a larger root
 * font size is clipped with an ellipsis rather than cut mid-glyph at the
 * sidebar edge.
 */
async function labelMetrics(page: Page, scope: string): Promise<LabelMetrics[]> {
  return page.evaluate((scopeSelector) => {
    return Array.from(
      document.querySelectorAll<HTMLElement>(
        `${scopeSelector} .primary-nav a span, ${scopeSelector} .nav-label`,
      ),
    ).map((node) => {
      const style = getComputedStyle(node)
      return {
        text: node.textContent ?? '',
        clientWidth: node.clientWidth,
        scrollWidth: node.scrollWidth,
        textOverflow: style.textOverflow,
        overflow: style.overflow,
      }
    })
  }, scope)
}

function expectLabelsFit(labels: LabelMetrics[]) {
  expect(labels.length, 'sidebar rendered labels').toBeGreaterThan(0)
  expect(
    labels.filter((label) => label.scrollWidth > label.clientWidth).map((label) => label.text),
    'labels whose text is wider than their box',
  ).toEqual([])
  expect(
    labels
      .filter((label) => label.textOverflow !== 'ellipsis' || label.overflow !== 'hidden')
      .map((label) => label.text),
    'labels without an ellipsis contract',
  ).toEqual([])
}

async function expectNoHorizontalBodyScroll(page: Page) {
  const overflow = await page.evaluate(() => ({
    clientWidth: document.documentElement.clientWidth,
    scrollWidth: document.documentElement.scrollWidth,
  }))
  expect(overflow.scrollWidth, 'document element horizontal overflow').toBeLessThanOrEqual(
    overflow.clientWidth,
  )
}

async function openSidebar(page: Page, locale: string, width: number, height: number) {
  await page.addInitScript((value) => {
    localStorage.setItem('ironrag.v1.locale', value)
  }, locale)
  await page.setViewportSize({ width, height })
  await openMockedApp(page, '/dashboard', { authenticated: true })
  await expect(page.locator('.sidebar .primary-nav a').first()).toBeVisible()
}

test('keeps every Russian sidebar label inside its box with an ellipsis contract', async ({
  page,
}) => {
  const issues = watchBrowserIssues(page)
  await openSidebar(page, 'ru', 1920, 1080)

  expectLabelsFit(await labelMetrics(page, '.sidebar'))
  await expectNoHorizontalBodyScroll(page)
  await page.screenshot({ path: 'test-results/sidebar-qa/ru-1920.png' })

  // Crowding at the two widths just above the drawer breakpoint: the sidebar is
  // still pinned, so a label that no longer fits would push the page sideways.
  for (const viewport of [
    { width: 900, height: 1080, shot: 'test-results/sidebar-qa/ru-900.png' },
    { width: 768, height: 1080, shot: null },
  ]) {
    await page.setViewportSize({ width: viewport.width, height: viewport.height })
    expectLabelsFit(await labelMetrics(page, '.sidebar'))
    await expectNoHorizontalBodyScroll(page)
    if (viewport.shot) await page.screenshot({ path: viewport.shot })
  }

  // The narrow layout replaces the sidebar with the mobile drawer, which is the
  // only place its labels are visible there.
  await page.setViewportSize({ width: 390, height: 844 })
  await page.locator('.topbar .mobile-menu').click()
  await expect(page.locator('.mobile-drawer')).toHaveClass(/is-open/u)
  // `.is-open` lands before the slide-in transition finishes; measure and
  // photograph the settled drawer, not a frame of its animation.
  await page.waitForFunction(
    () => Math.abs(document.querySelector('.mobile-drawer')?.getBoundingClientRect().left ?? 1) < 1,
  )
  expectLabelsFit(await labelMetrics(page, '.mobile-drawer'))
  await expectNoHorizontalBodyScroll(page)
  await page.screenshot({ path: 'test-results/sidebar-qa/ru-390-mobile.png' })

  await issues.assertClean()
})

test('keeps every English sidebar label inside its box with an ellipsis contract', async ({
  page,
}) => {
  const issues = watchBrowserIssues(page)
  await openSidebar(page, 'en', 1920, 1080)

  expectLabelsFit(await labelMetrics(page, '.sidebar'))
  await expectNoHorizontalBodyScroll(page)
  await page.screenshot({ path: 'test-results/sidebar-qa/en-1920.png' })

  await issues.assertClean()
})

test('ellipsizes sidebar labels when the root font outgrows the sidebar', async ({ page }) => {
  const issues = watchBrowserIssues(page)
  await openSidebar(page, 'ru', 1920, 1080)

  await page.addStyleTag({ content: 'html { font-size: 26px; }' })
  expect(await page.evaluate(() => getComputedStyle(document.documentElement).fontSize)).toBe(
    '26px',
  )

  const spills = await page.evaluate(() =>
    Array.from(document.querySelectorAll<HTMLElement>('.sidebar .primary-nav a span')).map(
      (node) => {
        const link = node.closest('a') as HTMLElement
        const linkRect = link.getBoundingClientRect()
        const paddingRight = Number.parseFloat(getComputedStyle(link).paddingRight)
        return {
          text: node.textContent ?? '',
          textOverflow: getComputedStyle(node).textOverflow,
          // Positive when the span crosses the link's own content edge, which a
          // flex child with `min-width: auto` does instead of shrinking.
          spill: node.getBoundingClientRect().right - (linkRect.right - paddingRight),
        }
      },
    ),
  )

  expect(spills.length, 'sidebar rendered links').toBeGreaterThan(0)
  expect(
    spills.filter((span) => span.spill > 0.5).map((span) => span.text),
    'spans crossing their link content box',
  ).toEqual([])
  expect(
    spills.filter((span) => span.textOverflow !== 'ellipsis').map((span) => span.text),
    'spans without an ellipsis contract',
  ).toEqual([])

  await issues.assertClean()
})

test('keeps every collapsed link named by its title with its span hidden', async ({ page }) => {
  const issues = watchBrowserIssues(page)
  await page.addInitScript(() => {
    localStorage.setItem('ironrag.v1.sidebar-collapsed', 'true')
  })
  await page.addInitScript(() => {
    localStorage.setItem('ironrag.v1.locale', 'ru')
  })
  await page.setViewportSize({ width: 1920, height: 1080 })
  await openMockedApp(page, '/dashboard', { authenticated: true })

  await expect(page.locator('.app-shell--collapsed')).toHaveCount(1)
  const links = page.locator('.primary-nav a')
  expect(await links.count()).toBeGreaterThan(0)

  // `title` is the only accessible name left once the span is hidden, so it is
  // the thing a collapse change must never drop.
  const titles = await links.evaluateAll((nodes) => nodes.map((node) => node.getAttribute('title')))
  expect(titles.filter((title) => title === null || title === '')).toEqual([])

  const spanDisplays = await page
    .locator('.primary-nav a span')
    .evaluateAll((nodes) => nodes.map((node) => getComputedStyle(node).display))
  expect(spanDisplays.length).toBeGreaterThan(0)
  expect(spanDisplays.filter((display) => display !== 'none')).toEqual([])

  await issues.assertClean()
})
