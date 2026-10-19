import type { Page } from '@playwright/test'
import { describe, expect, it, vi } from 'vitest'
import { watchBrowserIssues } from '../tests/e2e/mock-app'

function issueLog() {
  const listeners = new Map<string, (event: unknown) => void>()
  const page = {
    on: vi.fn((event: string, listener: (value: unknown) => void) =>
      listeners.set(event, listener),
    ),
    waitForTimeout: vi.fn().mockResolvedValue(undefined),
  }
  return { log: watchBrowserIssues(page as unknown as Page), listeners }
}

function failedRequest(errorText: string) {
  return {
    url: () => 'http://127.0.0.1/v1/content/libraries/library/documents',
    method: () => 'GET',
    failure: () => ({ errorText }),
  }
}

describe('browser API error evidence', () => {
  it('does not mistake a navigation/query cancellation for a network failure', async () => {
    const { log, listeners } = issueLog()
    listeners.get('requestfailed')!(failedRequest('net::ERR_ABORTED'))
    await expect(log.assertClean()).resolves.toBeUndefined()
  })

  it.each(['net::ERR_CONNECTION_REFUSED', 'net::ERR_FAILED', ''])(
    'keeps %s fatal',
    async (error) => {
      const { log, listeners } = issueLog()
      listeners.get('requestfailed')!(failedRequest(error))
      await expect(log.assertClean()).rejects.toThrow()
    },
  )

  it('keeps a rejected HTTP response fatal', async () => {
    const { log, listeners } = issueLog()
    const request = failedRequest('')
    listeners.get('response')!({ url: request.url, status: () => 403, request: () => request })
    await expect(log.assertClean()).rejects.toThrow()
  })

  it('does not silently accept an aborted mutation', async () => {
    const { log, listeners } = issueLog()
    listeners.get('requestfailed')!({ ...failedRequest('net::ERR_ABORTED'), method: () => 'POST' })
    await expect(log.assertClean()).rejects.toThrow()
  })
})
