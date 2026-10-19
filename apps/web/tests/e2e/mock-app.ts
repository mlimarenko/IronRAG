import { expect, type Page } from '@playwright/test'

import type { BrowserMockConfig } from '../../src/shared/api/mocks/e2e'

type BrowserIssueLog = {
  assertClean: () => Promise<void>
}

export function watchBrowserIssues(
  page: Page,
  { readOnly = false }: { readOnly?: boolean } = {},
): BrowserIssueLog {
  const issues: string[] = []

  if (readOnly) {
    page.on('request', (request) => {
      const path = new URL(request.url()).pathname
      if (path.startsWith('/v1/') && !['GET', 'HEAD'].includes(request.method())) {
        issues.push(`write: ${request.method()} ${path}`)
      }
    })
  }
  page.on('console', (message) => {
    if (message.type() === 'error') issues.push(`console: ${message.text()}`)
  })
  page.on('pageerror', (error) => issues.push(`page: ${error.message}`))
  page.on('requestfailed', (request) => {
    // Reloads and TanStack scope cancellation intentionally abort in-flight
    // GETs. Other transport failures (including unknown ones) remain errors.
    const errorText = request.failure()?.errorText ?? ''
    if (request.method() === 'GET' && errorText === 'net::ERR_ABORTED') return
    if (new URL(request.url()).pathname.startsWith('/v1/')) {
      issues.push(`request: ${request.method()} ${request.url()} ${errorText}`)
    }
  })
  page.on('response', (response) => {
    if (new URL(response.url()).pathname.startsWith('/v1/') && response.status() >= 400) {
      issues.push(`response: ${response.status()} ${response.request().method()} ${response.url()}`)
    }
  })

  return {
    async assertClean() {
      // Let query observers, route transitions, and deferred imports settle so
      // late API/console failures cannot escape an assertion made immediately
      // after the visible UI first appears.
      await page.waitForTimeout(300)
      expect(issues).toEqual([])
    },
  }
}

export async function openMockedApp(page: Page, path: string, config: BrowserMockConfig) {
  await page.addInitScript((mockConfig) => {
    window.__IRONRAG_E2E_MOCKS__ = mockConfig
  }, config)

  const separator = path.includes('?') ? '&' : '?'
  await page.goto(`${path}${separator}mocks=1`)
  await expect.poll(() => page.evaluate(() => window.__IRONRAG_MSW_READY__)).toBe(true)
}
