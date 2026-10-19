import { expect, test } from '@playwright/test'
import type { Page } from '@playwright/test'

const workspaceId = process.env.QA_WORKSPACE_ID?.trim()
const libraryId = process.env.QA_LIBRARY_ID?.trim()
const assistantQuestion = process.env.QA_ASSISTANT_QUESTION?.trim()
const expectedAnswerText = process.env.QA_ASSISTANT_EXPECTED_TEXT?.trim()
const timeoutMs = Number(process.env.QA_ASSISTANT_TIMEOUT_MS ?? 180_000)
const isConfigured = Boolean(workspaceId && libraryId && assistantQuestion && expectedAnswerText)

function collectRuntimeErrors(page: Page) {
  const runtimeErrors: string[] = []

  page.on('pageerror', (error) => {
    runtimeErrors.push(error.stack ?? error.message)
  })

  page.on('console', (message) => {
    if (message.type() === 'error') runtimeErrors.push(message.text())
  })

  return runtimeErrors
}

test.use({
  trace: 'retain-on-failure',
  screenshot: 'only-on-failure',
})

test.describe('assistant smoke (live backend)', () => {
  // This release-gate suite requires an explicitly configured live target; ordinary CI has none.
  test.skip(
    !isConfigured,
    'Set QA_WORKSPACE_ID, QA_LIBRARY_ID, QA_ASSISTANT_QUESTION, and QA_ASSISTANT_EXPECTED_TEXT to run the live assistant smoke test.',
  )

  test('returns a visible answer without a terminal request error', async ({ page }) => {
    test.setTimeout(timeoutMs + 30_000)
    const runtimeErrors = collectRuntimeErrors(page)

    await page.addInitScript(
      ({ selectedWorkspaceId, selectedLibraryId }) => {
        localStorage.setItem('ironrag.v1.active-workspace', selectedWorkspaceId)
        localStorage.setItem('ironrag.v1.active-library', selectedLibraryId)
      },
      { selectedWorkspaceId: workspaceId!, selectedLibraryId: libraryId! },
    )
    const selectedLibrarySessionsPromise = page.waitForResponse(
      (response) =>
        response.request().method() === 'GET' &&
        new URL(response.url()).pathname === `/v1/query/libraries/${libraryId}/sessions`,
    )
    await page.goto('/assistant', { waitUntil: 'domcontentloaded' })
    const selectedLibrarySessions = await selectedLibrarySessionsPromise
    expect(selectedLibrarySessions.ok()).toBe(true)
    await expect(page.getByRole('heading', { name: /Вход|Login/i })).toHaveCount(0)

    const editor = page.getByRole('textbox', { name: /Задайте вопрос|Ask a question/i })
    await editor.fill(assistantQuestion!)

    const turnResponsePromise = page.waitForResponse(
      (response) =>
        response.request().method() === 'POST' &&
        /\/v1\/query\/sessions\/[^/]+\/turns$/.test(new URL(response.url()).pathname),
      { timeout: 30_000 },
    )

    const sendButton = page.getByRole('button', { name: /Отправить|Send/i })
    if (await sendButton.count()) await sendButton.click()
    else await editor.press('Enter')

    const turnResponse = await turnResponsePromise
    expect(turnResponse.ok()).toBe(true)

    await expect(page.getByRole('button', { name: /Копировать|Copy/i })).toBeVisible({
      timeout: timeoutMs,
    })
    await expect(page.getByText(expectedAnswerText!, { exact: false }).last()).toBeVisible()
    await expect(page.getByText(/Запрос не прошёл|Request failed/i)).toHaveCount(0)
    await expect(page.getByText(/internal server error/i)).toHaveCount(0)
    expect(runtimeErrors).toEqual([])
  })
})
