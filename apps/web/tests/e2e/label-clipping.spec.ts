import { expect, test, type Page } from '@playwright/test'
import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'

import { openMockedApp, watchBrowserIssues } from './mock-app'

const componentsCss = readFileSync(resolve(process.cwd(), 'src/styles/components.css'), 'utf8')
const adminCss = readFileSync(resolve(process.cwd(), 'src/styles/admin.css'), 'utf8')

const BADGE_LABEL = 'Недоступно: провайдер отключён'

/**
 * A squeezed model-name column: a flex row narrow enough that
 * the badge has to shrink. `.fixed` is the shipped `<Badge>` output — the label
 * is a real child, so `text-overflow` lands on the box that holds the text.
 * `.legacy` is the same badge with a bare text child, which is an anonymous
 * flex item and therefore hard-clips even with the ellipsis declared on the
 * badge; it is the rendered control that proves which one buys the ellipsis.
 */
const fixture = `
  <body>
    <div class="clip-row fixed">
      <span class="badge badge--warning"><span class="badge__label">${BADGE_LABEL}</span></span>
    </div>
    <div class="clip-row legacy">
      <span class="badge badge--warning">${BADGE_LABEL}</span>
    </div>
  </body>
`

async function openFixture(page: Page, width: number, height: number) {
  await page.setViewportSize({ width, height })
  await page.setContent(`<!doctype html><style>${componentsCss}
    body { margin: 0; }
    .clip-row { display: flex; align-items: center; gap: 7px; width: 110px; margin: 8px; }
  </style>${fixture}`)
}

async function labelMetrics(page: Page) {
  return page.locator('.fixed .badge__label').evaluate((node) => {
    const style = getComputedStyle(node)
    return {
      clientWidth: (node as HTMLElement).clientWidth,
      scrollWidth: (node as HTMLElement).scrollWidth,
      textOverflow: style.textOverflow,
      overflow: style.overflow,
    }
  })
}

test('a squeezed badge ellipsizes its label instead of cutting it mid-glyph', async ({ page }) => {
  await openFixture(page, 1280, 720)

  const metrics = await labelMetrics(page)
  expect(metrics.scrollWidth, 'label text wider than its box').toBeGreaterThan(metrics.clientWidth)
  expect(metrics.textOverflow).toBe('ellipsis')
  expect(metrics.overflow).toBe('hidden')

  // The control keeps a bare text child: no label box, so no ellipsis. This is
  // what the fixed markup buys, not the declaration on the badge alone.
  await expect(page.locator('.legacy .badge__label')).toHaveCount(0)

  await page.screenshot({ path: 'test-results/label-qa/badge-desktop-1280.png' })
})

test('the badge stays inside a narrow viewport without sideways scroll', async ({ page }) => {
  await openFixture(page, 390, 844)

  const metrics = await labelMetrics(page)
  expect(metrics.scrollWidth).toBeGreaterThan(metrics.clientWidth)
  expect(metrics.textOverflow).toBe('ellipsis')

  const overflow = await page.evaluate(() => ({
    clientWidth: document.documentElement.clientWidth,
    scrollWidth: document.documentElement.scrollWidth,
  }))
  expect(overflow.scrollWidth, 'document element horizontal overflow').toBeLessThanOrEqual(
    overflow.clientWidth,
  )

  await page.screenshot({ path: 'test-results/label-qa/badge-narrow-390.png' })
})

async function expectMcpActionLabelsFit(page: Page) {
  const spills = await page
    .locator('.client-configs summary, .system-prompt-header')
    .evaluateAll((rows) =>
      rows.flatMap((row) => {
        const container = row.closest<HTMLElement>('.client-configs details, .system-prompt')!
        // A focused descendant can scroll even an overflow:hidden ancestor.
        // Measure the initial view, not that accidental programmatic rescue.
        container.scrollLeft = 0
        const bounds = container.getBoundingClientRect()
        const left = bounds.left + container.clientLeft
        const right = left + container.clientWidth
        const walker = document.createTreeWalker(row, NodeFilter.SHOW_TEXT)
        const clipped: string[] = []
        while (walker.nextNode()) {
          const text = walker.currentNode
          if (!text.textContent?.trim()) continue
          const range = document.createRange()
          range.selectNodeContents(text)
          if (
            Array.from(range.getClientRects()).some(
              (rect) => rect.left < left - 0.5 || rect.right > right + 0.5,
            )
          ) {
            clipped.push(text.textContent)
          }
        }
        return clipped
      }),
    )
  expect(spills, 'MCP header text clipped by its container').toEqual([])
  const overflow = await page.evaluate(() => ({
    clientWidth: document.documentElement.clientWidth,
    scrollWidth: document.documentElement.scrollWidth,
  }))
  expect(overflow.scrollWidth).toBeLessThanOrEqual(overflow.clientWidth)
}

test('MCP header wrapping preserves loading and error card spacing', async ({ page }) => {
  await page.setContent(`<!doctype html><style>${componentsCss}\n${adminCss}</style>
    <section class="system-prompt">
      <div class="system-prompt-header"><span>System prompt</span></div>
      <div class="state-card aicfg-loading-state"><strong>Loading</strong></div>
      <div class="state-card state-card--error"><div><strong>Unavailable</strong></div></div>
    </section>`)
  for (const width of [1280, 390]) {
    await page.setViewportSize({ width, height: 844 })
    for (const card of await page.locator('.system-prompt .state-card').all()) {
      await expect(card).toHaveCSS('padding', '24px')
      await expect(card).toHaveCSS('gap', '16px')
      await expect(card).toHaveCSS('min-height', '180px')
    }
    await page.locator('.system-prompt').screenshot({
      path: `test-results/label-qa/mcp-prompt-states-${width}.png`,
    })
  }
})

for (const locale of ['ru', 'en']) {
  for (const fontSize of [16, 26]) {
    test(`MCP action labels remain readable in ${locale} at ${fontSize}px`, async ({ page }) => {
      const issues = watchBrowserIssues(page)
      await page.addInitScript((value) => {
        localStorage.setItem('ironrag.v1.locale', value)
      }, locale)
      await openMockedApp(page, '/admin/system', { authenticated: true })
      const promptCopyButton = page.locator('.system-prompt-header .button')
      await expect(promptCopyButton).toBeVisible()
      await expect(promptCopyButton).toHaveAccessibleName(locale === 'ru' ? 'Копировать' : 'Copy')
      const copyButtons = page.locator('.client-configs summary .button')
      expect(await copyButtons.count()).toBeGreaterThan(0)
      const copyLabel = locale === 'ru' ? 'Копировать конфигурацию' : 'Copy configuration'
      for (const button of await copyButtons.all()) {
        await expect(button).toHaveAccessibleName(copyLabel)
      }
      await page.addStyleTag({ content: `html { font-size: ${fontSize}px; }` })

      for (const width of [1920, 900, 768, 390]) {
        await page.setViewportSize({ width, height: 1080 })
        await expectMcpActionLabelsFit(page)
        if (width === 1920 || width === 390) {
          await page.locator('.client-configs').screenshot({
            path: `test-results/label-qa/mcp-${locale}-${fontSize}-${width}.png`,
          })
          await page.locator('.system-prompt').screenshot({
            path: `test-results/label-qa/mcp-prompt-${locale}-${fontSize}-${width}.png`,
          })
        }
      }

      const summary = page.locator('.client-configs summary').first()
      await summary.focus()
      await page.keyboard.press('Enter')
      await expect(page.locator('.client-configs details').first()).toHaveAttribute('open', '')
      await expectMcpActionLabelsFit(page)
      await issues.assertClean()
    })
  }
}
