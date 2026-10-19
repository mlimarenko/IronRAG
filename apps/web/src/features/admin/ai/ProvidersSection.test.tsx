import { act, cleanup, render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import i18n from '@/shared/i18n'
import type { AIAccount, AIModelOption, AIProvider } from '@/shared/types'
import { ProvidersSection } from './ProvidersSection'

vi.mock('./api', () => {
  const mutation = { isPending: false, mutateAsync: vi.fn() }
  return {
    useAiAdminMutations: () => ({
      providerCreate: mutation,
      providerUpdate: mutation,
      providerDelete: mutation,
    }),
    useProviderDetail: () => ({ data: undefined, isPending: false }),
  }
})

const provider: AIProvider = {
  id: 'provider-alpha',
  displayName: 'Provider Alpha',
  kind: 'alpha',
  apiStyle: 'chat-completions',
  lifecycleState: 'active',
  defaultBaseUrl: 'https://provider.example/v1',
  apiKeyRequired: true,
  baseUrlRequired: false,
  credentialPolicy: {
    apiKeyRequired: true,
    baseUrlRequired: false,
    baseUrlMode: 'fixed',
    validationMode: 'model_list',
  },
  baseUrlPolicy: {
    allowOverride: false,
    requireHttps: true,
    allowPrivateNetwork: false,
    trimSuffixes: [],
  },
  modelDiscovery: { mode: 'credential', paths: [] },
  capabilities: {},
  runtime: {},
  uiHints: {},
  purposeGaps: [],
  modelCount: 0,
  credentialCount: 0,
}

function model(id: string): AIModelOption {
  return {
    id,
    providerCatalogId: provider.id,
    modelName: id,
    allowedBindingPurposes: [],
    availabilityState: 'available',
    availableAccountIds: [],
    seeded: true,
  }
}

function account(id: string): AIAccount {
  return {
    id,
    scopeKind: 'instance',
    providerId: provider.id,
    providerName: provider.displayName,
    providerKind: provider.kind,
    label: id,
    state: 'active',
    createdAt: '2026-01-01T00:00:00Z',
    updatedAt: '2026-01-01T00:00:00Z',
    apiKeySummary: 'configured',
  }
}

function renderUsage(modelCount: number, accountCount: number) {
  render(
    <ProvidersSection
      providers={[provider]}
      models={Array.from({ length: modelCount }, (_, index) => model(`model-${index}`))}
      accounts={Array.from({ length: accountCount }, (_, index) => account(`account-${index}`))}
    />,
  )
}

describe('ProvidersSection provider usage pluralisation', () => {
  beforeEach(async () => {
    await act(async () => {
      await i18n.changeLanguage('en')
    })
  })

  afterEach(async () => {
    // Unmount first: a mounted screen would receive the language event after
    // the test returns, which React reports as an update outside act().
    cleanup()
    await act(async () => {
      await i18n.changeLanguage('en')
    })
  })

  it('keeps capabilities and destructive actions in expandable details', async () => {
    const user = userEvent.setup()
    renderUsage(2, 1)
    const row = screen.getByRole('listitem', { name: provider.displayName })
    expect(row.querySelector('.ai-provider-profile')).not.toBeVisible()
    expect(screen.getByRole('button', { name: `Edit: ${provider.displayName}` })).toBeVisible()
    expect(
      screen.queryByRole('button', { name: `Delete: ${provider.displayName}` }),
    ).not.toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: provider.displayName }))
    expect(row.querySelector('.ai-provider-profile')).toBeVisible()
    expect(screen.getByRole('button', { name: `Delete: ${provider.displayName}` })).toBeVisible()
  })

  it.each([
    [1, 1, '1 model · 1 account'],
    [2, 2, '2 models · 2 accounts'],
  ])('renders %i models and %i accounts in English', (models, accounts, expected) => {
    renderUsage(models, accounts)

    expect(screen.getByText(expected)).toBeInTheDocument()
  })

  it.each([
    [1, 1, '1 модель · 1 аккаунт'],
    [2, 2, '2 модели · 2 аккаунта'],
    [5, 5, '5 моделей · 5 аккаунтов'],
  ])('renders %i models and %i accounts in Russian', async (models, accounts, expected) => {
    await act(async () => {
      await i18n.changeLanguage('ru')
    })
    renderUsage(models, accounts)

    expect(screen.getByText(expected)).toBeInTheDocument()
  })
})
