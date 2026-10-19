import { expect, test, type Locator, type Page } from '@playwright/test'

import type {
  AiAccountResponse,
  AiBindingResponse,
  ModelCatalogEntryResponse,
  ProviderCatalogEntryResponse,
} from '../../src/shared/api/generated'
import type { AiMockConfig } from '../../src/shared/api/mocks/e2e'
import { BINDING_PURPOSES } from '../../src/features/admin/model/aiConfig'
import { openMockedApp } from './mock-app'

const PROVIDER_ID = 'provider-mock'
const ACCOUNT_ID = 'account-mock'
const MODEL_ID = 'model-mock'

const provider: ProviderCatalogEntryResponse = {
  apiKeyRequired: true,
  apiStyle: 'chat-completions',
  baseUrlPolicy: {
    allowOverride: false,
    allowPrivateNetwork: false,
    requireHttps: true,
    trimSuffixes: [],
  },
  baseUrlRequired: false,
  capabilities: {
    chat: 'supported',
    embeddings: 'supported',
    modelDiscovery: 'supported',
    streaming: 'supported',
    tools: 'supported',
    vision: 'unsupported',
  },
  credentialPolicy: {
    apiKeyRequired: true,
    baseUrlMode: 'fixed',
    baseUrlRequired: false,
    validationMode: 'model_list',
  },
  defaultBaseUrl: 'https://provider.example/v1',
  displayName: 'Mock Provider',
  id: PROVIDER_ID,
  lifecycleState: 'active',
  modelDiscovery: { mode: 'shared', paths: [] },
  providerKind: 'openai-compatible',
  purposeGaps: [],
  runtime: {
    authScheme: 'bearer',
    chatPath: '/chat/completions',
    embeddingsPath: '/embeddings',
    kind: 'openai_compatible',
    modelsPath: '/models',
    structuredOutput: 'json_object',
    tokenLimitParameter: 'max_tokens',
  },
  uiHints: {},
}

const model: ModelCatalogEntryResponse = {
  allowedBindingPurposes: [
    'extract_text',
    'extract_graph',
    'embed_chunk',
    'query_compile',
    'query_answer',
    'agent',
  ],
  availabilityState: 'available',
  availableAccountIds: [ACCOUNT_ID],
  contextWindow: 8192,
  id: MODEL_ID,
  lifecycleState: 'active',
  maxOutputTokens: 2048,
  metadataJson: {},
  modelName: 'mock/chat',
  pricing: {
    cacheWriteInputPricePer1m: '0',
    cachedInputPricePer1m: '0',
    currencyCode: 'USD',
    inputPricePer1m: '0',
    outputPricePer1m: '0',
  },
  providerCatalogId: PROVIDER_ID,
}

/**
 * The override dialog's body must overflow, or the scroll assertion below
 * proves nothing. The picker renders the catalog rows from `/v1/ai/models`
 * (the `available-models` handler stays empty and is not this list's source),
 * and the model picker has a paginated table. One model leaves the body only
 * marginally taller than its viewport; twelve models exercise the compact
 * page limit, and expanding the parameters makes overflow deterministic.
 */
const models: ModelCatalogEntryResponse[] = [
  model,
  ...Array.from({ length: 11 }, (_, index) => ({
    ...model,
    id: `${MODEL_ID}-${index + 2}`,
    modelName: `mock/chat-${index + 2}`,
  })),
]

const account: AiAccountResponse = {
  apiKeySummary: 'configured',
  createdAt: '2026-01-01T00:00:00Z',
  credentialState: 'active',
  id: ACCOUNT_ID,
  label: 'Mock account',
  providerCatalogId: PROVIDER_ID,
  scopeKind: 'instance',
  updatedAt: '2026-01-01T00:00:00Z',
}

/** The library inherits this instance binding; editing creates a local override. */
const instanceBinding: AiBindingResponse = {
  accountId: ACCOUNT_ID,
  bindingPurpose: 'query_answer',
  bindingState: 'active',
  extraParametersJson: null,
  id: 'binding-instance-1',
  modelCatalogId: MODEL_ID,
  providerCatalogId: PROVIDER_ID,
  scopeKind: 'instance',
}

const ai: AiMockConfig = {
  providers: [provider],
  models,
  accounts: [account],
  bindings: [instanceBinding],
}

async function openOverrideDialog(page: Page, locale = 'en') {
  await page
    .getByRole('listitem', { name: locale === 'ru' ? 'Генерация ответов' : 'Answer Generation' })
    .getByRole('button', { name: locale === 'ru' ? 'Изменить' : 'Edit' })
    .click()
  const dialog = page.getByRole('dialog')
  await expect(dialog).toBeVisible()
  return dialog
}

/** The Save button must be painted at its own centre, not clipped by an ancestor. */
async function expectReachable(save: Locator) {
  await expect(save).toBeVisible()
  await expect(save).toBeEnabled()
  const hit = await save.evaluate((element) => {
    const rect = element.getBoundingClientRect()
    const target = document.elementFromPoint(rect.left + rect.width / 2, rect.top + rect.height / 2)
    return element === target || element.contains(target)
  })
  expect(hit).toBe(true)
}

for (const { width, theme, locale } of [
  { width: 1440, theme: 'light' },
  { width: 1440, theme: 'dark' },
  { width: 390, theme: 'light' },
  { width: 390, theme: 'dark' },
].flatMap((view) => ['en', 'ru'].map((locale) => ({ ...view, locale })))) {
  test(`keeps compact AI settings and model selection readable in ${locale} ${theme} mode at ${width}px`, async ({
    page,
  }) => {
    await page.setViewportSize({ width, height: width === 390 ? 844 : 1000 })
    await page.addInitScript(
      ({ theme, locale }) => {
        localStorage.setItem('ironrag.v1.theme', JSON.stringify(theme))
        localStorage.setItem('ironrag.v1.locale', locale)
      },
      { theme, locale },
    )
    await openMockedApp(page, '/admin/ai?scope=library&section=bindings', {
      authenticated: true,
      ai: {
        ...ai,
        models: models.map((entry) => ({
          ...entry,
          allowedBindingPurposes: [...BINDING_PURPOSES],
        })),
        bindings: BINDING_PURPOSES.map((purpose) => ({
          ...instanceBinding,
          id: `binding-${purpose}`,
          bindingPurpose: purpose,
        })),
      },
    })
    const row = page.getByRole('listitem', {
      name: locale === 'ru' ? 'Генерация ответов' : 'Answer Generation',
    })
    await expect(
      row.getByRole('button', { name: locale === 'ru' ? 'Изменить' : 'Edit' }),
    ).toBeVisible()
    await expect(page.locator('html')).toHaveAttribute('data-theme', theme)
    const scopeStart = await page.locator('.inheritance-ladder > button').first().boundingBox()
    expect(scopeStart?.height).toBeLessThan(36)
    await expect(page.locator('.settings-row')).toHaveCount(8)
    const heights = await page
      .locator('.settings-row')
      .evaluateAll((rows) => rows.map((element) => element.getBoundingClientRect().height))
    expect(Math.max(...heights)).toBeLessThan(width === 390 ? 150 : 90)
    await expect(row.locator('.settings-row__toggle')).toHaveAttribute('aria-expanded', 'false')
    await expect(row).toContainText(
      locale === 'ru' ? 'Наследуется · Инстанс' : 'Inherited · Instance',
    )
    await page.screenshot({
      path: `test-results/e2e/ai-bindings-${locale}-${theme}-${width}.png`,
      fullPage: true,
    })
    await row.locator('.settings-row__toggle').focus()
    await page.keyboard.press('Enter')
    await expect(row.locator('.settings-row__toggle')).toHaveAttribute('aria-expanded', 'true')
    await expect(row.locator('.settings-row__details p')).toBeVisible()
    await page.keyboard.press('Enter')
    await expect(row.locator('.settings-row__toggle')).toHaveAttribute('aria-expanded', 'false')
    const dialog = await openOverrideDialog(page, locale)
    await expect(dialog.locator('.model-picker-field__selection')).toContainText('mock/chat')
    await expect(
      dialog.getByRole('button', { name: locale === 'ru' ? /Подходят/ : /Fit the purpose/ }),
    ).toHaveAttribute('aria-pressed', 'true')
    await expectReachable(
      dialog
        .locator('.dialog-footer')
        .getByRole('button', { name: locale === 'ru' ? 'Сохранить' : 'Save' }),
    )
    const bodyWidth = await page.evaluate(() => document.documentElement.scrollWidth)
    expect(bodyWidth).toBe(width)
    await page.screenshot({
      path: `test-results/e2e/ai-model-choice-${locale}-${theme}-${width}.png`,
    })
    await page.keyboard.press('Escape')
    await page
      .getByRole('tab', { name: locale === 'ru' ? 'Аккаунты' : 'Accounts', exact: true })
      .click()
    const accountRow = page.getByRole('listitem', { name: account.label, exact: true })
    await expect(accountRow).toBeVisible()
    await expect(accountRow).toHaveAttribute('data-slot', 'settings-row')
    await expect(accountRow.locator('.settings-row__details')).toBeHidden()
    await expect(accountRow.locator('.settings-row__actions button')).toHaveCount(0)
    await page.screenshot({ path: `test-results/e2e/ai-accounts-${locale}-${theme}-${width}.png` })
    await accountRow.getByRole('button', { name: account.label, exact: true }).click()
    await expect(
      accountRow.getByText(provider.defaultBaseUrl as string, { exact: true }),
    ).toBeVisible()
    await expect(accountRow.locator('.settings-row__details button')).toHaveCount(0)
    await page
      .getByRole('tab', { name: locale === 'ru' ? 'Каталог' : 'Catalog', exact: true })
      .click()
    const modelRow = page.getByRole('listitem', { name: model.modelName, exact: true })
    await expect(modelRow).toBeVisible()
    await expect(modelRow.locator('.ai-record-facts')).toBeHidden()
    const catalogHeights = await page
      .locator('.settings-row')
      .evaluateAll((rows) => rows.map((element) => element.getBoundingClientRect().height))
    expect(Math.max(...catalogHeights)).toBeLessThan(150)
    await page.screenshot({ path: `test-results/e2e/ai-catalog-${locale}-${theme}-${width}.png` })
    await modelRow.locator('.settings-row__toggle').click()
    await expect(modelRow.locator('.ai-record-facts')).toBeVisible()
    expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBe(width)
    await page.screenshot({
      path: `test-results/e2e/ai-catalog-details-${locale}-${theme}-${width}.png`,
    })
    await modelRow.locator('.settings-row__actions button').click()
    await expect(page.getByRole('dialog')).toBeVisible()
    await expectReachable(
      page
        .getByRole('dialog')
        .getByRole('button', { name: locale === 'ru' ? 'Сохранить' : 'Save' }),
    )
    await page.keyboard.press('Escape')
    await page
      .getByRole('tab', { name: locale === 'ru' ? 'Модели' : 'Models', exact: true })
      .focus()
    await page.keyboard.press('ArrowRight')
    await expect(
      page.getByRole('tab', { name: locale === 'ru' ? 'Провайдеры' : 'Providers', exact: true }),
    ).toHaveAttribute('aria-selected', 'true')
    await expect(page).toHaveURL(/catalog=providers/)
    const providerRow = page.getByRole('listitem', { name: provider.displayName, exact: true })
    await expect(providerRow).toBeVisible()
    await expect(providerRow.locator('.ai-provider-profile')).toBeHidden()
    await page.screenshot({ path: `test-results/e2e/ai-providers-${locale}-${theme}-${width}.png` })
    await providerRow.locator('.settings-row__toggle').focus()
    await page.keyboard.press('Enter')
    await expect(providerRow.locator('.ai-provider-profile')).toBeVisible()
    expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBe(width)
    await page.screenshot({
      path: `test-results/e2e/ai-provider-details-${locale}-${theme}-${width}.png`,
    })
  })
}

test('keeps dialog actions in the footer and reachable on a tall viewport', async ({ page }) => {
  await page.setViewportSize({ width: 1920, height: 1440 })
  await openMockedApp(page, '/admin/ai?scope=library&section=bindings', {
    authenticated: true,
    ai,
  })

  await expect(page.getByRole('listitem', { name: 'Answer Generation' })).toBeVisible()
  await page.screenshot({
    path: 'test-results/e2e/ai-bindings-desktop-1920x1440.png',
    fullPage: true,
  })
  const dialog = await openOverrideDialog(page)
  await page.screenshot({ path: 'test-results/e2e/ai-model-choice-desktop-1920x1440.png' })
  await dialog.getByText('Model and parameters', { exact: true }).click()
  const footer = dialog.locator('.dialog-footer')
  await expect(footer).toHaveCount(1)
  await expect(dialog.locator('.dialog-body .dialog-footer')).toHaveCount(0)

  const save = footer.getByRole('button', { name: 'Save' })
  await expect(dialog.locator('.dialog-body').getByRole('button', { name: 'Save' })).toHaveCount(0)

  const body = dialog.locator('.dialog-body')
  // Precondition: the fixture must overflow the body viewport. If a layout
  // change ever lets the content fit, fail here with a clear reason instead of
  // silently reading `scrollTop === 0` as a missing scroll region.
  const overflow = await body.evaluate((element) => ({
    clientHeight: element.clientHeight,
    scrollHeight: element.scrollHeight,
  }))
  expect(overflow.scrollHeight).toBeGreaterThan(overflow.clientHeight)

  const scrolled = await body.evaluate((element) => {
    element.scrollTop = element.scrollHeight
    return element.scrollTop
  })
  expect(scrolled).toBeGreaterThan(0)

  await expectReachable(save)
  await page.screenshot({ path: 'test-results/e2e/dialog-actions-tall-1920x1440.png' })
})

test('model picker uses the common searchable table and numeric column menu', async ({
  page,
}, testInfo) => {
  await page.setViewportSize({ width: 390, height: 844 })
  await openMockedApp(page, '/admin/ai?scope=library&section=bindings', {
    authenticated: true,
    ai: {
      ...ai,
      models: models.map((item, index) => ({ ...item, contextWindow: 8192 + index * 512 })),
    },
  })
  const dialog = await openOverrideDialog(page)
  const table = dialog.getByRole('table')
  await expect(table).toBeVisible()
  await expect(table.getByRole('row')).toHaveCount(11)
  await expect(dialog.getByRole('button', { name: /Fit the purpose/ })).toHaveAttribute(
    'aria-pressed',
    'true',
  )
  const search = dialog.getByRole('searchbox')
  await expect(search).toHaveCount(1)
  await search.fill('8192')
  await expect(table.getByText('mock/chat', { exact: true })).toBeVisible()
  await expect(table.getByText('mock/chat-2', { exact: true })).toHaveCount(0)
  await search.fill('')
  const context = table.getByRole('columnheader', { name: /Context/i })
  await context.getByRole('button').click()
  await page.getByRole('menuitem', { name: /descending/i }).click()
  await expect(context).toHaveAttribute('aria-sort', 'descending')
  await expect(table.getByRole('row').nth(1)).toContainText('mock/chat-12')
  await page.screenshot({
    path: testInfo.outputPath('model-picker-grid-mobile.png'),
    fullPage: true,
  })
  await table
    .getByRole('row')
    .nth(1)
    .getByRole('button', { name: /mock\/chat-12/ })
    .click()
  await expect(dialog.locator('.model-picker-field__selection')).toContainText('mock/chat-12')
  await search.fill('mock/chat-2')
  await expect(dialog.locator('.model-picker-field__selection')).toContainText('mock/chat-12')
  await expect(table.getByText('mock/chat-12', { exact: true })).toHaveCount(0)
  await expect(dialog.getByRole('button', { name: 'Save' })).toBeEnabled()
})

test('keeps dialog actions reachable at a narrow viewport', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 })
  await openMockedApp(page, '/admin/ai?scope=library&section=bindings', {
    authenticated: true,
    ai,
  })

  await expect(page.getByRole('listitem', { name: 'Answer Generation' })).toBeVisible()
  const readiness = await page.locator('.ai-readiness').boundingBox()
  expect(readiness?.height).toBeLessThan(300)
  await page.screenshot({ path: 'test-results/e2e/ai-bindings-narrow-390x844.png', fullPage: true })
  const dialog = await openOverrideDialog(page)
  const footer = dialog.locator('.dialog-footer')
  await expect(footer).toHaveCount(1)

  const save = footer.getByRole('button', { name: 'Save' })
  await dialog.locator('.dialog-body').evaluate((element) => {
    element.scrollTop = element.scrollHeight
  })

  await expectReachable(save)
  await page.screenshot({ path: 'test-results/e2e/dialog-actions-narrow-390x844.png' })
  await page.keyboard.press('Escape')
  await page.getByRole('tab', { name: 'Accounts', exact: true }).click()
  await expect(page.getByText('Mock account', { exact: true })).toBeVisible()
  await expect(page.getByText('Inherited · Instance · Read-only')).toBeVisible()
  await page.screenshot({ path: 'test-results/e2e/ai-accounts-narrow-390x844.png', fullPage: true })
  await page.getByRole('tab', { name: 'Catalog', exact: true }).click()
  await expect(page.getByRole('listitem', { name: 'mock/chat', exact: true })).toBeVisible()
  await page.screenshot({ path: 'test-results/e2e/ai-catalog-narrow-390x844.png', fullPage: true })
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBe(390)
})

test('reaches the dialog actions with the keyboard', async ({ page }) => {
  await page.setViewportSize({ width: 1920, height: 1440 })
  await openMockedApp(page, '/admin/ai?scope=library&section=bindings', {
    authenticated: true,
    ai,
  })

  const dialog = await openOverrideDialog(page)
  const save = dialog.locator('.dialog-footer').getByRole('button', { name: 'Save' })

  // The dialog must confine Tab, and the footer must sit in the focus order
  // behind the body: reaching Save cannot depend on scrolling the body by hand.
  let reached = false
  for (let step = 0; step < 80; step += 1) {
    if (await save.evaluate((element) => element === document.activeElement)) {
      reached = true
      break
    }
    await page.keyboard.press('Tab')
  }
  expect(reached).toBe(true)
  await expect(save).toBeFocused()
  await expectReachable(save)

  // Keyboard close, the way a keyboard-only reader leaves the dialog.
  await page.keyboard.press('Escape')
  await expect(dialog).toHaveCount(0)
})

for (const locale of ['en', 'ru'] as const) {
  for (const width of [1440, 390]) {
    test(`requires a fresh reasoning decision in ${locale} at ${width}px`, async ({
      page,
    }, testInfo) => {
      await page.setViewportSize({ width, height: width === 390 ? 844 : 1000 })
      await page.addInitScript(
        (language) => localStorage.setItem('ironrag.v1.locale', language),
        locale,
      )
      await openMockedApp(page, '/admin/ai?scope=library&section=bindings', {
        authenticated: true,
        ai: {
          ...ai,
          models: models.slice(0, 2).map((entry, index) => ({
            ...entry,
            metadataJson: {
              reasoningCapability: {
                confirmed: {
                  mode: 'none',
                  wire: index === 0 ? 'reasoning_effort_none' : 'thinking_disabled',
                  evidence: 'synthetic browser fixture, not vendor acceptance',
                },
              },
            },
          })),
          bindings: [
            {
              ...instanceBinding,
              id: 'local-reasoning-binding',
              scopeKind: 'library',
              workspaceId: 'workspace-alpha',
              libraryId: 'library-demo-1',
              extraParametersJson: { reasoningMode: { version: 2, mode: 'none' } },
            },
          ],
        },
      })
      const dialog = await openOverrideDialog(page, locale)
      const parameters = dialog.locator('.ai-binding-parameters')
      await expect(dialog.locator('.validation-history__heading')).toHaveCSS('display', 'flex')
      await expect(parameters).toHaveAttribute('open', '')
      await expect(dialog.getByRole('alert')).toBeVisible()
      const unset = locale === 'en' ? 'Unset' : 'Сбросить'
      await dialog.getByRole('button', { name: unset, exact: true }).click()
      const selector = dialog.getByLabel(locale === 'en' ? 'Reasoning mode' : 'Режим рассуждения', {
        exact: true,
      })
      // Resolving the error may close the optional tuning disclosure.
      if ((await parameters.getAttribute('open')) === null) {
        await parameters.locator('summary').click()
      }
      await selector.selectOption('none')
      await dialog.getByRole('button', { name: /mock\/chat-2/ }).click()
      const save = dialog.getByRole('button', {
        name: locale === 'en' ? 'Save' : 'Сохранить',
        exact: true,
      })
      await expect(save).toBeDisabled()
      const confirm = dialog.getByRole('button', {
        name: locale === 'en' ? 'Confirm for this pair' : 'Подтвердить для этой пары',
        exact: true,
      })
      await expect(confirm).toBeVisible()
      await confirm.focus()
      await page.screenshot({
        path: testInfo.outputPath(`reasoning-confirm-${locale}-${width}.png`),
      })
      await page.keyboard.press('Enter')
      await expect(save).toBeEnabled()
      await expect(selector).toHaveValue('none')
      if ((await parameters.getAttribute('open')) === null) {
        await parameters.locator('summary').click()
      }
      await expect(selector).toBeVisible()
      await selector.scrollIntoViewIfNeeded()
      expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBe(width)
      await page.screenshot({ path: testInfo.outputPath(`reasoning-${locale}-${width}.png`) })
    })
  }
}
