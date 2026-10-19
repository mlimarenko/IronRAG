import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import i18n from '@/shared/i18n'
import type { AIAccount, AIProvider } from '@/shared/types'
import type { AdminModelOption } from '../model/aiAdapter'
import { AiWizard } from './AiWizard'

const mocks = vi.hoisted(() => ({
  accountCreate: {
    isPending: false,
    mutateAsync: vi.fn(),
  },
  bindingCreate: {
    isPending: false,
    mutateAsync: vi.fn(),
  },
}))

vi.mock('./api', () => ({
  useAiAdminMutations: () => mocks,
}))

const provider: AIProvider = {
  id: 'provider-1',
  displayName: 'Provider One',
  purposeGaps: [],
  kind: 'openai-compatible',
  apiStyle: 'chat-completions',
  lifecycleState: 'active',
  defaultBaseUrl: 'https://provider.example/v1',
  apiKeyRequired: false,
  baseUrlRequired: false,
  credentialPolicy: {
    apiKeyRequired: false,
    baseUrlRequired: false,
    baseUrlMode: 'optional',
    validationMode: 'model_list',
  },
  baseUrlPolicy: {
    allowOverride: false,
    requireHttps: true,
    allowPrivateNetwork: false,
    trimSuffixes: [],
  },
  modelDiscovery: {
    mode: 'shared',
    paths: [],
  },
  capabilities: {},
  runtime: {},
  uiHints: {},
  modelCount: 1,
  credentialCount: 1,
}

const account: AIAccount = {
  id: 'account-1',
  scopeKind: 'instance',
  providerId: provider.id,
  providerName: provider.displayName,
  providerKind: provider.kind,
  provider,
  label: 'Primary account',
  state: 'active',
  createdAt: '2026-08-11T00:00:00Z',
  updatedAt: '2026-08-11T00:00:00Z',
  apiKeySummary: '',
}

const model: AdminModelOption = {
  id: 'model-1',
  providerCatalogId: provider.id,
  modelName: 'Model One',
  allowedBindingPurposes: ['query_answer'],
  availabilityState: 'available',
  availableAccountIds: [account.id],
  reasoningCapability: { state: 'unconfirmed' },
  seeded: true,
  pricing: {
    cacheWriteInputPricePer1m: '0',
    cachedInputPricePer1m: '0',
    currencyCode: 'USD',
    inputPricePer1m: '0.20',
    outputPricePer1m: '1.20',
  },
}

function renderWizard() {
  return render(
    <AiWizard
      open
      onOpenChange={vi.fn()}
      selectedScope="instance"
      onScopeChange={vi.fn()}
      scopeContext={{}}
      accounts={[account]}
      providers={[provider]}
      models={[model]}
    />,
  )
}

async function advanceToAccountStep() {
  const user = userEvent.setup()
  await user.click(screen.getByRole('button', { name: i18n.t('admin.aiWizard.continue') }))
  await user.click(screen.getByRole('button', { name: i18n.t('admin.aiWizard.continue') }))
  return user
}

describe('AiWizard', () => {
  beforeEach(async () => {
    vi.clearAllMocks()
    await i18n.changeLanguage('en')
  })

  it('formats model prices with the shared money format and translated summary', async () => {
    renderWizard()
    const user = await advanceToAccountStep()

    await user.selectOptions(screen.getByRole('combobox', { name: 'Accounts' }), account.id)
    await user.click(screen.getByRole('button', { name: 'Continue' }))

    expect(screen.getByText('input USD 0.20 / output USD 1.20 per 1M tokens')).toBeInTheDocument()
  })

  it('translates the model pricing summary for Russian operators', async () => {
    await i18n.changeLanguage('ru')
    renderWizard()
    const user = await advanceToAccountStep()

    await user.selectOptions(
      screen.getByRole('combobox', { name: i18n.t('admin.accounts') }),
      account.id,
    )
    await user.click(screen.getByRole('button', { name: i18n.t('admin.aiWizard.continue') }))

    expect(screen.getByText('вход 0,20 USD / выход 1,20 USD за 1M токенов')).toBeInTheDocument()
  })

  it('explains why the provider Base URL cannot be edited', async () => {
    renderWizard()
    const user = await advanceToAccountStep()

    await user.click(screen.getByRole('button', { name: 'Create new account' }))
    await user.selectOptions(screen.getByRole('combobox', { name: 'Provider' }), provider.id)

    const baseUrlField = screen.getByRole('textbox', { name: /^Base URL/ })
    expect(baseUrlField).toBeDisabled()
    expect(baseUrlField.closest('label')).toHaveTextContent(
      'This endpoint is fixed by the provider catalog.',
    )
  })
})
