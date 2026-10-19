import { expect, test } from '@playwright/test'

import type { AuditEventResponse } from '../../src/shared/api/generated'
import type { BrowserMockConfig } from '../../src/shared/api/mocks/e2e'
import { catalogLibrary } from '../../src/shared/api/mocks/fixtures/catalog-library'
import { openMockedApp, watchBrowserIssues } from './mock-app'

const surfaces = ['rest', 'mcp', 'worker']

// 26 events: with a page of 25 the needle actor is on the second page, so a
// matching search proves the whole dataset is searched, not the page in view.
const auditItems: AuditEventResponse[] = Array.from({ length: 26 }, (_, index) => {
  const needle = index === 25
  const systemActor = index % 5 === 0 && !needle
  return {
    id: `audit-${String(index).padStart(2, '0')}`,
    createdAt: `2026-09-${String(26 - index).padStart(2, '0')}T10:00:00Z`,
    actionKind: needle ? 'audit.fixture.needle' : `audit.fixture.event_${index}`,
    actorPrincipalId: systemActor ? null : `actor-${index}`,
    ...(needle
      ? {
          assistantCall: {
            queryExecutionId: 'query-needle',
            models: [{ providerKind: 'openai_compatible', modelName: 'atlas' }],
            totalCost: '0.125',
            currencyCode: 'USD',
            providerCallCount: 2,
          },
        }
      : {}),
    ...(systemActor
      ? {}
      : {
          actorPrincipal: {
            id: `actor-${index}`,
            principalKind: 'user',
            status: 'active',
            displayLabel: needle ? 'Needle User' : `Actor ${String(index).padStart(2, '0')}`,
          },
        }),
    resultKind: index % 4 === 0 ? 'failed' : 'succeeded',
    surfaceKind: surfaces[index % surfaces.length] ?? 'rest',
    redactedMessage: index % 4 === 0 ? `boom failure ${'detail_token_'.repeat(40)}` : 'ok',
    subjects: [],
  }
})

const audit: NonNullable<BrowserMockConfig['audit']> = { items: auditItems }

for (const [viewport, width, theme] of [
  ['desktop-light', 1440, 'light'],
  ['desktop-dark', 1440, 'dark'],
  ['mobile-light', 390, 'light'],
  ['mobile-dark', 390, 'dark'],
] as const) {
  test(`audit grid searches the whole set, sorts and value-filters on ${viewport}`, async ({
    page,
  }, testInfo) => {
    const issues = watchBrowserIssues(page, { readOnly: true })
    await page.setViewportSize({ width, height: 844 })
    await page.addInitScript((theme) => {
      localStorage.setItem('ironrag.v1.theme', JSON.stringify(theme))
    }, theme)
    await openMockedApp(page, '/admin/audit?workspaceId=workspace-alpha&pageSize=25', {
      authenticated: true,
      audit,
      workspaces: [
        {
          id: 'workspace-alpha',
          slug: 'workspace-alpha',
          name: 'Operational reference workspace with a long descriptive scope name',
        },
      ],
      libraries: [
        catalogLibrary({
          displayName: 'Operational reference library with a long descriptive scope name',
        }),
      ],
    })
    await expect(page.locator('html')).toHaveAttribute('data-theme', theme)

    const table = page.getByRole('table')
    await expect(table).toBeVisible()
    await expect(table.locator('tbody tr')).toHaveCount(25)
    // Hidden document overflow is not containment: controls must fit the actual
    // viewport, and rich audit details must leave all desktop columns visible.
    const layout = await page.evaluate(() => {
      const controls = [
        ...document.querySelectorAll('.data-grid__toolbar input, .data-grid__toolbar select'),
      ].filter((element) => element.getAttribute('type') !== 'checkbox')
      const scroll = document.querySelector('.data-grid__scroll')!
      return {
        controls: controls.map((element) => {
          const { left, right } = element.getBoundingClientRect()
          return { left, right }
        }),
        tableWidth: scroll.scrollWidth,
        availableWidth: scroll.clientWidth,
      }
    })
    for (const control of layout.controls) {
      expect(control.left).toBeGreaterThanOrEqual(0)
      expect(control.right).toBeLessThanOrEqual(width)
    }
    if (width === 1440) expect(layout.tableWidth).toBeLessThanOrEqual(layout.availableWidth)
    expect(
      await page.evaluate(() => ({
        width: innerWidth,
        scroll: document.documentElement.scrollWidth,
      })),
    ).toEqual({ width, scroll: width })

    // The needle belongs to a row beyond the first 25.
    const search = page.getByRole('searchbox').first()
    await search.fill('Needle User')
    await expect(table.getByText('Needle User')).toBeVisible()
    await expect(table.getByText('Actor 24', { exact: true })).toHaveCount(0)
    await expect(table.getByText('openai_compatible:atlas')).toBeVisible()
    await expect(table.getByText('USD 0.125 · 2 calls')).toBeVisible()
    const inclusion = page.getByRole('checkbox', { name: 'Assistant model and cost' })
    await page.getByText('Assistant model and cost', { exact: true }).click()
    await expect(inclusion).not.toBeChecked()
    await expect(table.getByText('openai_compatible:atlas')).toHaveCount(0)
    await inclusion.focus()
    await inclusion.press('Space')
    await expect(inclusion).toBeChecked()
    await expect(table.getByText('openai_compatible:atlas')).toBeVisible()
    await page.screenshot({
      path: testInfo.outputPath(`audit-assistant-${viewport}.png`),
      fullPage: true,
    })
    // A copied displayed second resolves to the off-page timestamp through
    // typed hints, not through a coincidentally matching ISO date string.
    const displayedDate = await table.locator('time').textContent()
    expect(displayedDate).toBeTruthy()
    const dateRequest = page.waitForRequest((request) => {
      const url = new URL(request.url())
      return url.pathname === '/v1/audit/events' && url.searchParams.get('search') === displayedDate
    })
    await search.fill(displayedDate!)
    const dateParams = new URL((await dateRequest).url()).searchParams
    expect(JSON.parse(dateParams.get('searchHints')!)).toHaveProperty('createdAtRange')
    await expect(table.getByText('Needle User')).toBeVisible()
    await expect(table.locator('tbody tr')).toHaveCount(1)
    await search.fill('')

    // Every data header sorts; the header states the direction it was given.
    const actorHeader = table.getByRole('columnheader', { name: /Actor/i })
    await actorHeader.getByRole('button').click()
    await page.getByRole('menuitem', { name: /descending/i }).click()
    await expect(actorHeader).toHaveAttribute('aria-sort', 'descending')
    await page.keyboard.press('Escape')

    // Every data header filters by value over the whole filtered set.
    const resultHeader = table.getByRole('columnheader', { name: /Result/i })
    await resultHeader.getByRole('button').click()
    await page.getByRole('menuitemcheckbox', { name: /Failed/i }).click()
    await page.keyboard.press('Escape')
    await expect(table.getByText('boom failure').first()).toBeVisible()

    await page.screenshot({ path: testInfo.outputPath(`audit-${viewport}.png`), fullPage: true })
    await issues.assertClean()
  })
}

test('an audit column menu is keyboard reachable and Escape returns focus to its header', async ({
  page,
}) => {
  const issues = watchBrowserIssues(page, { readOnly: true })
  await openMockedApp(page, '/admin/audit', { authenticated: true, audit })
  const header = page.getByRole('table').getByRole('columnheader').first().getByRole('button')
  await header.focus()
  await page.keyboard.press('Enter')
  await expect(page.getByRole('menu')).toBeVisible()
  await page.keyboard.press('Escape')
  await expect(header).toBeFocused()
  await issues.assertClean()
})
