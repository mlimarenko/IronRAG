import { expect, test } from '@playwright/test'
import type { Page } from '@playwright/test'

const workspaceId = process.env.QA_WORKSPACE_ID?.trim()
const libraryId = process.env.QA_LIBRARY_ID?.trim()

if (Boolean(workspaceId) !== Boolean(libraryId)) {
  throw new Error('QA_WORKSPACE_ID and QA_LIBRARY_ID must be configured together')
}

/**
 * Visual QA against the live backend. Every scenario uses the authenticated
 * session persisted by `global-setup.ts` and waits for a page-specific final
 * state. Polling pages deliberately do not use `networkidle`: they may never
 * become idle, and reaching a quiet network does not prove that the UI left
 * its loading state.
 *
 * Screenshots land under `visual-qa/screenshots/{project}-{scenario}.png`.
 */

type Scenario = {
  name: string
  path: string
  ready: (page: Page) => Promise<void>
  requiresConfiguredLibraryRequest?: boolean
}

type RuntimeFailures = {
  browser: string[]
  api: string[]
}

const heading = (page: Page, name: RegExp) =>
  expect(page.getByRole('heading', { level: 1, name })).toBeVisible()

function isApiUrl(url: string): boolean {
  try {
    const pathname = new URL(url).pathname
    return pathname === '/v1' || pathname.startsWith('/v1/')
  } catch {
    return false
  }
}

function requestLabel(method: string, url: string): string {
  try {
    const parsed = new URL(url)
    return `${method} ${parsed.pathname}`
  } catch {
    return `${method} <invalid URL>`
  }
}

function collectRuntimeFailures(page: Page): RuntimeFailures {
  const failures: RuntimeFailures = { browser: [], api: [] }

  page.on('pageerror', (error) => {
    failures.browser.push(error.stack ?? error.message)
  })

  page.on('console', (message) => {
    if (message.type() !== 'error') return
    const text = message.text()
    if (text.includes('[vite] connecting') || text.includes('[vite] connected')) return
    failures.browser.push(text)
  })

  page.on('requestfailed', (request) => {
    if (!isApiUrl(request.url())) return
    failures.api.push(
      `${requestLabel(request.method(), request.url())} request failed: ${request.failure()?.errorText ?? 'unknown error'}`,
    )
  })

  page.on('response', (response) => {
    if (!isApiUrl(response.url()) || response.status() < 400) return
    failures.api.push(
      `${requestLabel(response.request().method(), response.url())} returned HTTP ${response.status()}`,
    )
  })

  return failures
}

async function expectAuthenticatedPage(page: Page) {
  await expect(page.getByRole('heading', { name: /Вход|Login/i })).toHaveCount(0)
}

async function configureScope(page: Page) {
  if (!workspaceId || !libraryId) return
  await page.addInitScript(
    ({ selectedWorkspaceId, selectedLibraryId }) => {
      localStorage.setItem('ironrag.v1.active-workspace', selectedWorkspaceId)
      localStorage.setItem('ironrag.v1.active-library', selectedLibraryId)
    },
    { selectedWorkspaceId: workspaceId, selectedLibraryId: libraryId },
  )
}

async function expectConfiguredScope(page: Page) {
  if (!workspaceId || !libraryId) return
  await expect
    .poll(() =>
      page.evaluate(() => ({
        workspaceId: localStorage.getItem('ironrag.v1.active-workspace'),
        libraryId: localStorage.getItem('ironrag.v1.active-library'),
      })),
    )
    .toEqual({ workspaceId, libraryId })
}

function waitForSessionResolution(page: Page) {
  return page.waitForResponse((response) => {
    const url = new URL(response.url())
    return response.request().method() === 'GET' && url.pathname === '/v1/iam/context'
  })
}

function observeConfiguredLibraryRequest(page: Page) {
  let observed = false
  page.on('request', (request) => {
    if (!libraryId) return
    const url = new URL(request.url())
    if (url.pathname.startsWith('/v1/iam/')) return
    observed ||=
      url.pathname.split('/').includes(libraryId) || url.searchParams.get('libraryId') === libraryId
  })
  return () => observed
}

async function expectLiveCoverageDonut(page: Page) {
  const donut = page.getByRole('progressbar', {
    name: /Share of all library documents ready for answers|Доля готовых к ответам документов во всей библиотеке/i,
  })
  await expect(donut).toBeVisible()
  await expect(donut).toHaveAttribute('aria-valuemin', '0')
  await expect(donut).toHaveAttribute('aria-valuemax', '100')
  const valueText = await donut.getAttribute('aria-valuenow')
  expect(valueText).toMatch(/^\d+$/u)
  const value = Number(valueText)
  expect(value).toBeGreaterThanOrEqual(0)
  expect(value).toBeLessThanOrEqual(100)
  await expect(donut).toContainText(`${value}%`)

  const visual = await donut.evaluate((element) => {
    const inline = element as HTMLElement
    return {
      coverage: inline.style.getPropertyValue('--coverage-percent'),
      backgroundImage: getComputedStyle(element).backgroundImage,
    }
  })
  expect(visual.coverage).toBe(`${value}%`)
  const stops = visual.backgroundImage.match(
    /^conic-gradient\((.+?) 0deg, \1 ([\d.]+)%, (.+?) ([\d.]+)%, \3 100%\)$/u,
  )
  expect(stops).not.toBeNull()
  expect(Number(stops?.[2])).toBe(value)
  expect(Number(stops?.[4])).toBe(value)
  expect(stops?.[1]).not.toBe(stops?.[3])
}

async function expectNoErrorState(page: Page) {
  await expect(page.locator('.state-card--error')).toHaveCount(0)
  await expect(page.getByText(/failed to render|не удалось отрисовать/i)).toHaveCount(0)
}

const scenarios: Scenario[] = [
  {
    name: 'dashboard',
    path: '/dashboard',
    requiresConfiguredLibraryRequest: true,
    ready: async (page) => {
      await expect(page.locator('.page-header .eyebrow')).toHaveText(/^Dashboard$|^Дашборд$/i)
      await expect(page.locator('.dashboard-skeleton')).toHaveCount(0, { timeout: 30_000 })
      if (libraryId) {
        await expect(page.locator('.dashboard-metrics')).toBeVisible()
        await expect(page.locator('.coverage-panel')).toBeVisible()
        await expectLiveCoverageDonut(page)
      } else {
        await expect(
          page.locator('.dashboard-metrics, .state-card:not(.state-card--error)').first(),
        ).toBeVisible()
      }
    },
  },
  {
    name: 'documents',
    path: '/documents',
    requiresConfiguredLibraryRequest: true,
    ready: async (page) => {
      await heading(page, /Documents|Документы/i)
      await expect(page.getByText(/Loading documents|Загрузка документов/i)).toHaveCount(0, {
        timeout: 30_000,
      })
      if (libraryId) {
        await expect(page.locator('.document-toolbar')).toBeVisible()
        await expect(
          page.locator('.document-table tbody tr:visible, .document-mobile-card:visible').first(),
        ).toBeVisible()
      } else {
        await expect(
          page.locator('.document-toolbar, .state-card:not(.state-card--error)').first(),
        ).toBeVisible()
      }
    },
  },
  {
    name: 'graph',
    path: '/graph',
    requiresConfiguredLibraryRequest: true,
    ready: async (page) => {
      await heading(page, /Graph|Граф/i)
      await expect(page.locator('.graph-loading')).toHaveCount(0, { timeout: 120_000 })
      if (libraryId) {
        await expect(page.locator('.graph-workbench')).toBeVisible({ timeout: 120_000 })
        await expect(page.locator('.graph-canvas__preparing')).toHaveCount(0, { timeout: 120_000 })
        await expect(page.locator('.graph-canvas__runtime canvas').first()).toBeVisible()
      } else {
        await expect(
          page
            .locator('.graph-workbench, .graph-page > .state-card:not(.state-card--error)')
            .first(),
        ).toBeVisible({ timeout: 120_000 })
      }
    },
  },
  {
    name: 'assistant',
    path: '/assistant',
    ready: async (page) => {
      await expect(page.locator('.assistant-layout .chat-surface')).toBeVisible()
      await expect(page.locator('.chat-surface .composer textarea')).toBeVisible()
      await expect(
        page.locator('.session-rail .session-group').filter({ hasText: /Loading|Загрузка/i }),
      ).toHaveCount(0, { timeout: 30_000 })
    },
  },
  {
    name: 'admin-libraries',
    path: '/admin/libraries',
    ready: async (page) => {
      await heading(page, /Libraries|Библиотеки/i)
      await expect(page.locator('.admin-metrics')).toBeVisible({ timeout: 30_000 })
      await expect(page.locator('.admin-fetching')).toHaveCount(0)
    },
  },
  {
    name: 'admin-queue',
    path: '/admin/queue',
    ready: async (page) => {
      await heading(page, /Processing queue|Очередь обработки/i)
      await expect(page.locator('.queue-summary')).toBeVisible({ timeout: 30_000 })
      await expect(page.locator('.queue-polling-indicator')).toHaveCount(0)
    },
  },
  {
    name: 'admin-ai-bindings',
    path: '/admin/ai?scope=library&section=bindings',
    requiresConfiguredLibraryRequest: true,
    ready: async (page) => {
      await heading(page, /Models and providers|Модели и провайдеры/i)
      await expect(page).toHaveURL(/[?&]section=bindings(?:&|$)/)
      await expect(page.locator('.aicfg-loading-state')).toHaveCount(0, { timeout: 30_000 })
      await expect(page.locator('.settings-list > .settings-row').first()).toBeVisible()
    },
  },
  {
    name: 'admin-ai-accounts',
    path: '/admin/ai?scope=library&section=accounts',
    requiresConfiguredLibraryRequest: true,
    ready: async (page) => {
      await heading(page, /Models and providers|Модели и провайдеры/i)
      await expect(page).toHaveURL(/[?&]section=accounts(?:&|$)/)
      await expect(page.locator('.aicfg-loading-state')).toHaveCount(0, { timeout: 30_000 })
      await expect(
        page.getByRole('heading', { level: 2, name: /Accounts in this scope|Учётные записи/i }),
      ).toBeVisible()
      // Stage bootstrap creates an instance account. It must remain visible as
      // inherited configuration while a library scope is selected.
      await expect(page.locator('.settings-list > .settings-row').first()).toBeVisible()
      const inheritedAccount = page
        .locator('.settings-list > .settings-row')
        .filter({ hasText: /Read-only|Только просмотр/i })
        .first()
      await expect(inheritedAccount).toBeVisible()
      await expect(inheritedAccount.getByRole('button')).toHaveCount(1)
      await expect(inheritedAccount.getByRole('button')).toHaveAttribute('aria-expanded', 'false')
    },
  },
  {
    name: 'admin-ai-catalog',
    path: '/admin/ai?scope=library&section=catalog',
    requiresConfiguredLibraryRequest: true,
    ready: async (page) => {
      await heading(page, /Models and providers|Модели и провайдеры/i)
      await expect(page).toHaveURL(/[?&]section=catalog(?:&|$)/)
      await expect(page.locator('.aicfg-loading-state')).toHaveCount(0, { timeout: 30_000 })
      await page.getByRole('tab', { name: /Providers|Провайдеры/i }).click()
      await expect(
        page.getByRole('heading', { level: 2, name: /Providers|Провайдеры/i }),
      ).toBeVisible()
      await expect(
        page
          .getByRole('list', { name: /Providers|Провайдеры/i })
          .locator('.settings-row')
          .first(),
      ).toBeVisible()
      await expect(
        page
          .getByRole('list', { name: /Providers|Провайдеры/i })
          .locator('.settings-row')
          .filter({ hasText: /[1-9][0-9]* (?:accounts?|аккаунт(?:а|ов)?)/i })
          .first(),
      ).toBeVisible()
    },
  },
  {
    name: 'admin-access',
    path: '/admin/access',
    ready: async (page) => {
      await heading(page, /API tokens|API-токены/i)
      await expect(page.locator('.aicfg-loading-state')).toHaveCount(0, { timeout: 30_000 })
      await expect(page.getByRole('searchbox')).toBeVisible()
    },
  },
  {
    name: 'admin-users',
    path: '/admin/users',
    ready: async (page) => {
      await heading(page, /Users|Пользователи/i)
      await expect(page.locator('.aicfg-loading-state')).toHaveCount(0, { timeout: 30_000 })
      await expect(page.locator('.aicfg-readable-labels > .toolbar')).toBeVisible()
    },
  },
  {
    name: 'admin-system',
    path: '/admin/system',
    ready: async (page) => {
      await heading(page, /System|Система/i)
      await expect(page.locator('.system-status-banner--loading')).toHaveCount(0, {
        timeout: 30_000,
      })
      await expect(page.locator('.health-list > div').first()).toBeVisible()
      await expect(page.locator('.mcp-capability-summary .badge')).toBeVisible()
      await expect(page.locator('.system-prompt details.aicfg-system-prompt')).toBeVisible()
    },
  },
]

test.describe('wave-2 visual QA (live backend)', () => {
  for (const scenario of scenarios) {
    test(scenario.name, async ({ page }, testInfo) => {
      test.setTimeout(scenario.name === 'graph' ? 180_000 : 60_000)
      const failures = collectRuntimeFailures(page)
      await configureScope(page)
      const sessionResolution = waitForSessionResolution(page)
      const configuredLibraryWasRequested = observeConfiguredLibraryRequest(page)
      await page.goto(scenario.path, { waitUntil: 'domcontentloaded' })
      expect((await sessionResolution).ok()).toBe(true)
      await scenario.ready(page)
      await expectConfiguredScope(page)
      if (libraryId && scenario.requiresConfiguredLibraryRequest) {
        expect(configuredLibraryWasRequested()).toBe(true)
      }
      await expectAuthenticatedPage(page)
      await expectNoErrorState(page)

      const fileName = `${testInfo.project.name}-${scenario.name}.png`
      const screenshot = await page.screenshot({
        path: `visual-qa/screenshots/${fileName}`,
        fullPage: true,
      })
      expect(screenshot).not.toHaveLength(0)
      expect(failures.browser).toEqual([])
      expect(failures.api).toEqual([])
    })
  }

  test('admin-library-detail', async ({ page }, testInfo) => {
    test.setTimeout(60_000)
    const failures = collectRuntimeFailures(page)
    await configureScope(page)
    const sessionResolution = waitForSessionResolution(page)
    await page.goto('/admin/libraries', { waitUntil: 'domcontentloaded' })
    expect((await sessionResolution).ok()).toBe(true)
    await heading(page, /Libraries|Библиотеки/i)
    await expect(page.locator('.admin-metrics')).toBeVisible({ timeout: 30_000 })
    await expectConfiguredScope(page)
    await expectAuthenticatedPage(page)

    const firstLibrary = page
      .locator(
        '.admin-libraries-table tbody .table-link:visible, .mobile-card-list .mobile-data-card:visible',
      )
      .first()
    await expect(firstLibrary).toBeVisible()
    await firstLibrary.click()

    const dialog = page.getByRole('dialog')
    await expect(dialog).toBeVisible()
    await expect(dialog.locator('.library-inspector-state')).toHaveCount(0, { timeout: 30_000 })
    await expect(dialog.locator('.library-settings-form')).toBeVisible()
    await expect(dialog.locator('.connector-health')).not.toContainText(/Loading|Загрузка/i, {
      timeout: 30_000,
    })
    await expectNoErrorState(page)

    const screenshot = await page.screenshot({
      path: `visual-qa/screenshots/${testInfo.project.name}-admin-library-detail.png`,
      fullPage: true,
    })
    expect(screenshot).not.toHaveLength(0)
    expect(failures.browser).toEqual([])
    expect(failures.api).toEqual([])
  })

  test('safe dashboard and document interactions', async ({ page }, testInfo) => {
    test.skip(testInfo.project.name !== 'desktop', 'One desktop pass covers non-mutating controls')
    test.setTimeout(90_000)
    const failures = collectRuntimeFailures(page)
    await configureScope(page)

    await page.goto('/dashboard', { waitUntil: 'domcontentloaded' })
    await expect(page.locator('.dashboard-skeleton')).toHaveCount(0, { timeout: 30_000 })
    const dashboardRefresh = page.waitForResponse(
      (response) =>
        response.request().method() === 'GET' &&
        response.url().includes('/v1/ops/libraries/') &&
        response.url().endsWith('/dashboard'),
    )
    await page.getByRole('button', { name: /Refresh|Обновить/i }).click()
    expect((await dashboardRefresh).ok()).toBe(true)
    await page.getByRole('button', { name: /Ask a question|Задать вопрос/i }).click()
    await expect(page).toHaveURL(/\/assistant$/)
    await expect(page.locator('.chat-surface .composer textarea')).toBeVisible()

    await page.goto('/documents', { waitUntil: 'domcontentloaded' })
    await expect(page.locator('.document-toolbar')).toBeVisible({ timeout: 30_000 })
    const documentRefresh = page.waitForResponse(
      (response) =>
        response.request().method() === 'GET' &&
        response.url().includes('/v1/content/libraries/') &&
        response.url().includes('/documents'),
    )
    await page
      .getByRole('button', { name: /Refresh|Обновить/i })
      .first()
      .click()
    expect((await documentRefresh).ok()).toBe(true)

    const detailColumns = page.locator('.detail-columns-button')
    await detailColumns.click()
    await expect(detailColumns).toHaveAttribute('aria-pressed', 'true')
    await detailColumns.click()
    await expect(detailColumns).toHaveAttribute('aria-pressed', 'false')

    const selection = page.getByRole('button', { name: /^Select$|^Выбрать$/i })
    await selection.click()
    await expect(selection).toHaveAttribute('aria-pressed', 'true')
    await selection.click()
    await expect(selection).toHaveAttribute('aria-pressed', 'false')

    const sort = page.getByRole('combobox', { name: /Sort|Сортировка/i })
    const currentSort = await sort.inputValue()
    const alternateSort = await sort
      .locator('option')
      .evaluateAll(
        (options, selected) =>
          options
            .map((option) => (option as HTMLOptionElement).value)
            .find((value) => value !== selected),
        currentSort,
      )
    expect(alternateSort).toBeTruthy()
    await sort.selectOption(alternateSort ?? currentSort)
    await expect(sort).toHaveValue(alternateSort ?? currentSort)
    await expectNoErrorState(page)
    expect(failures.browser).toEqual([])
    // This scenario deliberately navigates while dashboard/session polling is
    // active. Chromium reports those superseded GETs as ERR_ABORTED; all other
    // transport and HTTP failures remain actionable.
    expect(failures.api.filter((failure) => !failure.endsWith('net::ERR_ABORTED'))).toEqual([])
  })

  test('safe AI scope and catalog interactions', async ({ page }, testInfo) => {
    test.skip(testInfo.project.name !== 'desktop', 'One desktop pass covers non-mutating controls')
    test.setTimeout(90_000)
    const failures = collectRuntimeFailures(page)
    await configureScope(page)
    await page.goto('/admin/ai?scope=library&section=accounts', {
      waitUntil: 'domcontentloaded',
    })
    await expect(page.locator('.aicfg-loading-state')).toHaveCount(0, { timeout: 30_000 })
    const scopes = page.locator('.inheritance-ladder')
    const scopeButtons = scopes.getByRole('button')
    await scopeButtons.first().click()
    await expect(page).toHaveURL(/[?&]scope=instance(?:&|$)/)
    await expect(page.locator('.settings-list > .settings-row').first()).toBeVisible()
    await scopeButtons.last().click()
    await expect(page).toHaveURL(/[?&]scope=library(?:&|$)/)

    await page.getByRole('tab', { name: /Catalog|Каталог/i }).click()
    await expect(page).toHaveURL(/[?&]section=catalog(?:&|$)/)
    await page.getByRole('tab', { name: /Providers|Провайдеры/i }).click()
    await expect(
      page
        .getByRole('list', { name: /Providers|Провайдеры/i })
        .locator('.settings-row')
        .first(),
    ).toBeVisible()
    await expect(
      page
        .getByRole('list', { name: /Providers|Провайдеры/i })
        .locator('.settings-row')
        .filter({ hasText: /[1-9][0-9]* (?:accounts?|аккаунт(?:а|ов)?)/i })
        .first(),
    ).toBeVisible()
    await page.getByRole('tab', { name: /Accounts|Аккаунты/i }).click()
    await expect(page).toHaveURL(/[?&]section=accounts(?:&|$)/)
    await expect(page.locator('.settings-list > .settings-row').first()).toBeVisible()
    await expectNoErrorState(page)
    expect(failures.browser).toEqual([])
    expect(failures.api).toEqual([])
  })

  test('safe graph controls', async ({ page }, testInfo) => {
    test.skip(testInfo.project.name !== 'desktop', 'One desktop pass covers non-mutating controls')
    test.setTimeout(180_000)
    const failures = collectRuntimeFailures(page)
    await configureScope(page)
    await page.goto('/graph', { waitUntil: 'domcontentloaded' })
    await expect(page.locator('.graph-loading')).toHaveCount(0, { timeout: 120_000 })
    await expect(page.locator('.graph-canvas__runtime canvas').first()).toBeVisible({
      timeout: 120_000,
    })

    // The desktop legend is always visible, so its filter drawer trigger is
    // intentionally hidden at this breakpoint. Exercise the controls that are
    // actually available in the rendered toolbar.
    const density = page.getByRole('button', { name: /Edge density|Плотность связей/i })
    await expect(density).toHaveAttribute('aria-pressed', 'false')
    await density.click()
    await expect(density).toHaveAttribute('aria-pressed', 'true')
    await density.click()
    await expect(density).toHaveAttribute('aria-pressed', 'false')
    await page.locator('.graph-fit-button').click()
    await expect(page.locator('.graph-canvas__runtime canvas').first()).toBeVisible()
    await expectNoErrorState(page)
    expect(failures.browser).toEqual([])
    expect(failures.api).toEqual([])
  })
})
