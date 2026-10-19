import { fileURLToPath } from 'node:url'
import path from 'node:path'

import { expect, test, type Download, type Page } from '@playwright/test'

import type { DocumentsMockConfig } from '../../src/shared/api/mocks/e2e'
import { openMockedApp, watchBrowserIssues } from './mock-app'

// Ordinary readers see only the current readable revision. Both download
// entry points must request its bytes; visibility is rechecked on every request.
const DOCUMENT_ID = 'doc-revision-e2e'
const REVISION_2_ID = 'rev-e2e-2'

const CURRENT_BODY = 'CURRENT READABLE REVISION -- revised handbook introduction\n'

const SOURCE_PATH = `/v1/content/documents/${DOCUMENT_ID}/source`

// The generated API client picks how to parse a response body from its
// Content-Type (`getParseAs` in `client/utils.gen.ts`): only `application/*`
// (among other binary families) resolves to `'blob'`; `text/*` resolves to
// `'text'` (a string). That once broke downloads of any document the backend
// serves as `text/*`, and it is fixed in `a891d89e` (IRG-78): `download()`
// passes an explicit `parseAs: 'blob'`, so the parse mode no longer depends
// on the response's Content-Type at all.
//
// This spec still mocks `application/octet-stream` on purpose: it is about
// revision selection (IRG-63), and a mime type that needs no override keeps
// the two concerns from being tested through one another. The `text/*` path
// has its own coverage in
// `apps/web/src/features/documents/components/documents-page/useDocumentInspectorController.test.ts`.
const SOURCE_MIME_TYPE = 'application/octet-stream'

const specDir = path.dirname(fileURLToPath(import.meta.url))
const screenshotDir = path.resolve(specDir, '..', '..', 'test-results', 'e2e')
const DESKTOP_SCREENSHOT_PATH = path.join(screenshotDir, 'documents-revision-download-desktop.png')
const MOBILE_SCREENSHOT_PATH = path.join(screenshotDir, 'documents-revision-download-375x812.png')

function documentsMockConfig(sourceUnavailable = false): DocumentsMockConfig {
  return {
    document: {
      id: DOCUMENT_ID,
      fileName: 'handbook.txt',
      fileType: 'txt',
      mimeType: SOURCE_MIME_TYPE,
      body: CURRENT_BODY,
    },
    revisions: [
      {
        id: REVISION_2_ID,
        revisionNumber: 2,
        body: CURRENT_BODY,
        mimeType: SOURCE_MIME_TYPE,
        createdAt: '2026-08-20T09:00:00.000Z',
      },
    ],
    ...(sourceUnavailable ? { unavailableRevisionId: REVISION_2_ID } : {}),
  }
}

async function readDownloadText(download: Download): Promise<string> {
  const stream = await download.createReadStream()
  if (!stream) return ''
  const chunks: Buffer[] = []
  for await (const chunk of stream) chunks.push(chunk as Buffer)
  return Buffer.concat(chunks).toString('utf8')
}

/**
 * Triggers `action` and waits for the browser's native download it causes,
 * returning the suggested filename and the actual saved bytes. This is the
 * most direct evidence of what the operator would end up with on disk.
 *
 * Earlier attempts at this spec used a `text/plain` mock Content-Type, under
 * which `page.waitForEvent('download')` never fired at all: the app's
 * `URL.createObjectURL` call threw on a string argument before ever reaching
 * `anchor.click()` — that failure mode looked like a Playwright or
 * gesture-detection limitation but was in fact downstream of the mock's mime
 * type. That defect is fixed (see the `SOURCE_MIME_TYPE` comment above); the
 * note stays because the symptom — a download event that simply never
 * arrives — reads as a harness problem and costs an hour every time.
 */
async function triggerDownload(page: Page, action: () => Promise<void>) {
  const [download] = await Promise.all([page.waitForEvent('download'), action()])
  return { filename: download.suggestedFilename(), text: await readDownloadText(download) }
}

/**
 * Records the `revisionId` query parameter of every outgoing source request,
 * in order, for later assertion. Deliberately non-blocking: gating a
 * download trigger on a `page.waitForRequest()` for one specific revisionId
 * would hang for the full test timeout if the code under test sends the
 * wrong (or no) revisionId — turning a content-mismatch bug into an opaque
 * "timeout exceeded" failure instead of a direct, diagnostic assertion.
 * `page.waitForResponse(...).text()` was also tried for this and reads an
 * empty body here: MSW serves these responses through a Service Worker, and
 * Playwright's CDP-based response-body capture does not reliably see bodies
 * a Service Worker synthesized rather than fetched over the network. Request
 * URLs don't have that problem, so this only inspects the request side.
 */
function recordSourceRequests(page: Page): (string | null)[] {
  const revisionIds: (string | null)[] = []
  page.on('request', (request) => {
    if (request.url().includes(SOURCE_PATH)) {
      revisionIds.push(new URL(request.url()).searchParams.get('revisionId'))
    }
  })
  return revisionIds
}

async function openRevisionsTab(page: Page, sourceUnavailable = false) {
  await openMockedApp(page, `/documents?documentId=${DOCUMENT_ID}`, {
    authenticated: true,
    documents: documentsMockConfig(sourceUnavailable),
  })
  await expect(page.getByRole('heading', { name: 'handbook.txt' })).toBeVisible()
  await page
    .getByRole('group', { name: 'Details' })
    .getByRole('button', { name: 'Revisions', exact: true })
    .click()
}

test('downloads only the current readable revision through both source entry points', async ({
  page,
}) => {
  const issues = watchBrowserIssues(page)
  const sourceRequests = recordSourceRequests(page)
  await openRevisionsTab(page)

  const revision2Button = page.getByRole('button', { name: 'Download revision 2' })
  await expect(page.getByRole('button', { name: 'Download revision 1' })).toHaveCount(0)
  await expect(revision2Button).toBeVisible()

  const download2 = await triggerDownload(page, () => revision2Button.click())
  expect(download2.filename).toBe('handbook (v2).txt')
  expect(download2.text).toBe(CURRENT_BODY)
  expect(sourceRequests.at(-1)).toBe(REVISION_2_ID)

  // The document entry point omits the selector and resolves the same current revision.
  const currentDownload = await triggerDownload(page, () =>
    page.getByRole('button', { name: 'Download', exact: true }).click(),
  )
  expect(currentDownload.filename).toBe('handbook.txt')
  expect(currentDownload.text).toBe(CURRENT_BODY)
  expect(currentDownload.text).toBe(download2.text)
  expect(sourceRequests.at(-1)).toBeNull()

  const requestCount = sourceRequests.length
  const repeatedDownload = await triggerDownload(page, () => revision2Button.click())
  expect(repeatedDownload.text).toBe(CURRENT_BODY)
  expect(sourceRequests).toHaveLength(requestCount + 1)
  expect(sourceRequests.at(-1)).toBe(REVISION_2_ID)

  await issues.assertClean()
})

test('each revision row control is keyboard reachable and activatable with a distinguishable name', async ({
  page,
}) => {
  const issues = watchBrowserIssues(page)
  const sourceRequests = recordSourceRequests(page)
  await openRevisionsTab(page)

  const revision2Button = page.getByRole('button', { name: 'Download revision 2' })
  await expect(revision2Button).toBeVisible()

  // Every row's control must carry its own accessible name, not a shared
  // generic one, so a screen-reader user can tell rows apart by name alone.
  const revisionButtons = page.getByRole('button', { name: /^Download revision \d+$/u })
  await expect(revisionButtons).toHaveCount(1)
  const names = await revisionButtons.evaluateAll((buttons) =>
    buttons.map((button) => button.getAttribute('aria-label')),
  )
  expect(new Set(names).size).toBe(names.length)

  await revision2Button.focus()
  await expect(revision2Button).toBeFocused()

  const download = await triggerDownload(page, () => page.keyboard.press('Enter'))
  expect(download.filename).toBe('handbook (v2).txt')
  expect(download.text).toBe(CURRENT_BODY)
  expect(sourceRequests.at(-1)).toBe(REVISION_2_ID)

  await issues.assertClean()
})

test('shows the localized fallback message for an unavailable revision, never raw backend text', async ({
  page,
}) => {
  // The mocked 404 is an intentional /v1/ error response, so this test does
  // not call `issues.assertClean()` — that helper treats every /v1/ >=400
  // response as a hard failure, which is exactly what this scenario needs to
  // trigger.
  await openRevisionsTab(page, true)

  const unavailableButton = page.getByRole('button', { name: 'Download revision 2' })
  await expect(unavailableButton).toBeVisible()
  await unavailableButton.click()

  const toast = page.locator('[data-sonner-toast]')
  await expect(toast).toContainText('Failed to download this revision', { timeout: 10_000 })
  await expect(toast).not.toContainText('CANARY-RAW-BACKEND-DETAIL-DO-NOT-DISPLAY')
})

test('captures the revisions list at desktop and a constrained viewport', async ({ page }) => {
  const issues = watchBrowserIssues(page)
  await openRevisionsTab(page)
  await expect(page.getByRole('button', { name: 'Download revision 1' })).toHaveCount(0)
  await expect(page.getByRole('button', { name: 'Download revision 2' })).toBeVisible()

  await page.screenshot({ path: DESKTOP_SCREENSHOT_PATH })

  // Resize in place rather than reopening the inspector at 375px: below the
  // desktop breakpoint the document table is CSS-hidden in favor of
  // `.mobile-card-list`, which is a different entry point into the
  // inspector and not what this screenshot is verifying.
  await page.setViewportSize({ width: 375, height: 812 })
  await expect(page.getByRole('button', { name: 'Download revision 1' })).toHaveCount(0)
  await expect(page.getByRole('button', { name: 'Download revision 2' })).toBeVisible()

  await page.screenshot({ path: MOBILE_SCREENSHOT_PATH })

  await issues.assertClean()
})
