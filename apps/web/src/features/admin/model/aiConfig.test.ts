import { describe, expect, it } from 'vitest'

import type { AIAccount, AIBinding, AIModelOption, AIProvider, AIScopeKind } from '@/shared/types'
import type { AiBindingPurpose } from '@/shared/api/generated'
import {
  BINDING_PURPOSES,
  isBindingExecutable,
  isDecisionBindingPurpose,
  isProviderCredentialValidationFailure,
  isModelAvailableForAccount,
  modelSupportsBindingPurpose,
  providerServesPurpose,
  aiReadinessStage,
  reachableAiScope,
  recommendAiConfigSection,
  summarizeAiReadiness,
  suggestBindingSelection,
} from './aiConfig'

describe('isProviderCredentialValidationFailure', () => {
  it('uses the typed API error kind instead of diagnostic prose', () => {
    expect(
      isProviderCredentialValidationFailure({
        body: { errorKind: 'provider_credential_validation_transport_failed' },
        message: 'opaque',
      }),
    ).toBe(true)
    expect(
      isProviderCredentialValidationFailure({
        body: { errorKind: 'unrelated_failure' },
        message: 'provider credential validation failed',
      }),
    ).toBe(false)
  })
})

function provider(overrides: Partial<AIProvider> = {}): AIProvider {
  return {
    id: 'provider-alpha',
    displayName: 'Provider Alpha',
    purposeGaps: [],
    kind: 'alpha',
    apiStyle: 'openai_chat',
    lifecycleState: 'active',
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
    modelDiscovery: {
      mode: 'credential',
      paths: [{ capabilityKind: 'chat', path: '/models' }],
    },
    capabilities: {
      chat: 'supported',
      embeddings: 'supported',
      modelDiscovery: 'supported',
      streaming: 'supported',
      tools: 'supported',
      vision: 'supported',
    },
    runtime: {},
    uiHints: {},
    modelCount: 1,
    credentialCount: 1,
    ...overrides,
  }
}

function account(overrides: Partial<AIAccount> = {}): AIAccount {
  return {
    id: 'account-alpha',
    scopeKind: 'instance',
    providerId: 'provider-alpha',
    providerName: 'Provider Alpha',
    providerKind: 'alpha',
    provider: provider(),
    label: 'Account Alpha',
    state: 'active',
    createdAt: '2026-01-01T00:00:00Z',
    updatedAt: '2026-01-02T00:00:00Z',
    apiKeySummary: 'masked', // pragma: allowlist secret
    ...overrides,
  }
}

function model(overrides: Partial<AIModelOption> = {}): AIModelOption {
  return {
    id: 'model-alpha',
    providerCatalogId: 'provider-alpha',
    modelName: 'alpha-chat',
    allowedBindingPurposes: ['query_answer'],
    availabilityState: 'available',
    seeded: true,
    availableAccountIds: ['account-alpha'],
    ...overrides,
  }
}

function binding(purpose: AiBindingPurpose, overrides: Partial<AIBinding> = {}): AIBinding {
  return {
    id: `binding-${purpose}`,
    scopeKind: 'instance',
    purpose,
    accountId: 'account-alpha',
    modelCatalogId: 'model-alpha',
    state: 'active',
    ...overrides,
  }
}

function runtimePurposeBinding(purpose: AiBindingPurpose): AIBinding {
  return binding(purpose, {
    id: `binding-${purpose}`,
    purpose,
    modelCatalogId: `model-${purpose}`,
  })
}

function summaryInput(
  overrides: {
    selectedScope?: AIScopeKind
    availableAccounts?: AIAccount[]
    localAccounts?: AIAccount[]
    bindingsForScope?: AIBinding[]
    instanceBindings?: AIBinding[]
    workspaceBindings?: AIBinding[]
    models?: AIModelOption[]
    providers?: AIProvider[]
  } = {},
) {
  return {
    selectedScope: overrides.selectedScope ?? 'instance',
    availableAccounts: overrides.availableAccounts ?? [account()],
    localAccounts: overrides.localAccounts ?? [account()],
    bindingsForScope: overrides.bindingsForScope ?? [binding('query_answer')],
    instanceBindings: overrides.instanceBindings ?? [binding('query_answer')],
    workspaceBindings: overrides.workspaceBindings ?? [],
    models: overrides.models ?? [model()],
    providers: overrides.providers ?? [provider()],
  }
}

describe('summarizeAiReadiness', () => {
  it('counts effective inherited runtime bindings for child scopes', () => {
    const summary = summarizeAiReadiness(
      summaryInput({
        selectedScope: 'library',
        bindingsForScope: [],
        workspaceBindings: [
          binding('query_compile', {
            id: 'binding-workspace-query-compile',
            scopeKind: 'workspace',
            modelCatalogId: 'model-compile',
          }),
        ],
        instanceBindings: [binding('query_answer')],
        models: [
          model(),
          model({
            id: 'model-compile',
            modelName: 'alpha-compile',
            allowedBindingPurposes: ['query_compile'],
          }),
        ],
      }),
    )

    expect(summary.executableEffectiveBindings).toBe(2)
    expect(summary.localBindingCount).toBe(0)
    expect(summary.missingPurposes).toContain('embed_chunk')
    expect(summary.missingPurposes).not.toContain('query_answer')
    expect(summary.missingPurposes).not.toContain('query_compile')
  })

  it('counts workflow-specific knowledge purposes without a global required taxonomy', () => {
    const runtimePurposes: AiBindingPurpose[] = [
      'extract_graph',
      'embed_chunk',
      'query_compile',
      'query_answer',
    ]
    const summary = summarizeAiReadiness(
      summaryInput({
        bindingsForScope: runtimePurposes.map(runtimePurposeBinding),
        instanceBindings: runtimePurposes.map(runtimePurposeBinding),
        models: runtimePurposes.map((purpose) =>
          model({
            id: `model-${purpose}`,
            modelName: `${purpose}-model`,
            allowedBindingPurposes: [purpose],
          }),
        ),
      }),
    )

    expect(summary.totalPurposes).toBe(4)
    expect(summary.executableEffectiveBindings).toBe(4)
    expect(summary.missingPurposes).toEqual([])
  })

  it('requires an executable account/model pair before marking a purpose ready', () => {
    const summary = summarizeAiReadiness(
      summaryInput({
        availableAccounts: [account({ state: 'revoked' })],
        localAccounts: [account({ state: 'revoked' })],
        bindingsForScope: [binding('query_answer')],
        instanceBindings: [binding('query_answer')],
      }),
    )

    expect(summary.executableEffectiveBindings).toBe(0)
    expect(summary.missingPurposes).toContain('query_answer')
  })

  it('treats bindings with unavailable models as missing', () => {
    const summary = summarizeAiReadiness(
      summaryInput({
        models: [model({ availabilityState: 'unavailable' })],
      }),
    )

    expect(summary.executableEffectiveBindings).toBe(0)
    expect(summary.missingPurposes).toContain('query_answer')
  })

  it('keeps unchecked account-discovered models executable until an account-specific check disproves them', () => {
    const summary = summarizeAiReadiness(
      summaryInput({
        models: [model({ availabilityState: 'unknown' })],
      }),
    )

    expect(summary.executableEffectiveBindings).toBe(1)
    expect(summary.missingPurposes).not.toContain('query_answer')
  })

  it('recommends the next required section from missing readiness data', () => {
    expect(
      recommendAiConfigSection(
        summarizeAiReadiness(
          summaryInput({
            availableAccounts: [],
            localAccounts: [],
          }),
        ),
      ),
    ).toBe('accounts')

    expect(
      recommendAiConfigSection(
        summarizeAiReadiness(
          summaryInput({
            bindingsForScope: [],
            instanceBindings: [],
          }),
        ),
      ),
    ).toBe('bindings')
  })
})

describe('required binding purposes', () => {
  it('exposes all API purposes in their UI order without required/optional categories', () => {
    expect(BINDING_PURPOSES).toEqual([
      'extract_text',
      'extract_graph',
      'graph_entity_type',
      'embed_chunk',
      'query_compile',
      'query_rerank',
      'query_answer',
      'agent',
    ])
    expect(new Set(BINDING_PURPOSES).size).toBe(8)
  })

  it('offers a decision-only provider only for the decision purposes', () => {
    const decisionProvider = provider({
      id: 'provider-typesafe',
      capabilities: { decision: 'supported' },
    })
    expect(isDecisionBindingPurpose('query_rerank')).toBe(true)
    expect(isDecisionBindingPurpose('graph_entity_type')).toBe(true)
    expect(isDecisionBindingPurpose('query_answer')).toBe(false)
    expect(providerServesPurpose(decisionProvider, 'query_rerank')).toBe(true)
    expect(providerServesPurpose(decisionProvider, 'graph_entity_type')).toBe(true)
    expect(providerServesPurpose(decisionProvider, 'extract_graph')).toBe(false)
    expect(providerServesPurpose(account().provider, 'extract_graph')).toBe(true)
  })

  it('uses typed allowed purposes as the only model eligibility authority', () => {
    expect(
      modelSupportsBindingPurpose(
        model({ allowedBindingPurposes: ['extract_text'] }),
        'extract_text',
      ),
    ).toBe(true)
    expect(
      modelSupportsBindingPurpose(
        model({ allowedBindingPurposes: ['query_answer'] }),
        'extract_text',
      ),
    ).toBe(false)
  })

  it('does not reinterpret typed eligibility through provider or catalog metadata', () => {
    expect(
      isBindingExecutable({
        purpose: 'extract_text',
        account: account({
          provider: provider({
            capabilities: {
              chat: 'unknown',
              vision: 'unsupported',
              tools: 'unknown',
            },
          }),
        }),
        model: model({
          allowedBindingPurposes: ['extract_text'],
        }),
        modelsByAccountId: {},
      }),
    ).toBe(true)
    expect(
      isBindingExecutable({
        purpose: 'agent',
        account: account({
          provider: provider({
            capabilities: {
              chat: 'unknown',
              tools: 'unknown',
            },
          }),
        }),
        model: model({
          allowedBindingPurposes: ['agent'],
        }),
        modelsByAccountId: {},
      }),
    ).toBe(true)
  })

  it('keeps embed-chunk-only typed models executable for embedding', () => {
    expect(
      modelSupportsBindingPurpose(
        model({ allowedBindingPurposes: ['embed_chunk'] }),
        'embed_chunk',
      ),
    ).toBe(true)
  })
})

describe('isModelAvailableForAccount', () => {
  it('trusts unresolved catalog availability until account-scoped discovery returns', () => {
    expect(
      isModelAvailableForAccount(
        model({ availabilityState: 'unknown', availableAccountIds: ['account-alpha'] }),
        account(),
        {},
      ),
    ).toBe(true)
  })

  it('trusts account-scoped discovery when it has returned', () => {
    expect(
      isModelAvailableForAccount(
        model({ availabilityState: 'unknown', availableAccountIds: ['account-alpha'] }),
        account(),
        { 'account-alpha': [] },
      ),
    ).toBe(false)

    expect(
      isModelAvailableForAccount(
        model({ availabilityState: 'unknown', availableAccountIds: ['account-alpha'] }),
        account(),
        { 'account-alpha': [model()] },
      ),
    ).toBe(true)
  })
})

describe('suggestBindingSelection', () => {
  it('suggests a typed Agent pair without reinterpreting provider metadata', () => {
    const suggestion = suggestBindingSelection({
      purpose: 'agent',
      availableAccounts: [
        account({
          provider: provider({
            capabilities: {
              chat: 'supported',
              tools: 'unknown',
            },
          }),
        }),
      ],
      models: [model({ allowedBindingPurposes: ['agent'] })],
    })

    expect(suggestion).toEqual({
      accountId: 'account-alpha',
      modelCatalogId: 'model-alpha',
    })
  })

  it('prefills a compatible active account and model for the selected purpose', () => {
    const suggestion = suggestBindingSelection({
      purpose: 'query_answer',
      availableAccounts: [
        account({
          id: 'account-revoked',
          state: 'revoked',
          updatedAt: '2026-01-03T00:00:00Z',
        }),
        account(),
      ],
      models: [
        model({
          id: 'model-graph',
          allowedBindingPurposes: ['extract_graph'],
        }),
        model(),
      ],
    })

    expect(suggestion).toEqual({
      accountId: 'account-alpha',
      modelCatalogId: 'model-alpha',
    })
  })

  it('keeps an existing compatible local selection when reopening an editor', () => {
    const suggestion = suggestBindingSelection({
      purpose: 'query_answer',
      availableAccounts: [
        account(),
        account({
          id: 'account-beta',
          providerId: 'provider-beta',
          providerName: 'Provider Beta',
          providerKind: 'beta',
        }),
      ],
      models: [
        model(),
        model({
          id: 'model-beta',
          providerCatalogId: 'provider-beta',
          modelName: 'beta-chat',
          availableAccountIds: ['account-beta'],
        }),
      ],
      preferredAccountId: 'account-beta',
      preferredModelCatalogId: 'model-beta',
    })

    expect(suggestion).toEqual({
      accountId: 'account-beta',
      modelCatalogId: 'model-beta',
    })
  })

  it('leaves binding selectors empty when there is no active executable pair', () => {
    const suggestion = suggestBindingSelection({
      purpose: 'query_answer',
      availableAccounts: [account({ state: 'revoked' })],
      models: [model()],
    })

    expect(suggestion).toEqual({
      accountId: '',
      modelCatalogId: '',
    })
  })
})

describe('reachableAiScope', () => {
  it('keeps a scope that is actually selectable', () => {
    expect(reachableAiScope('library', true, true)).toBe('library')
    expect(reachableAiScope('workspace', true, false)).toBe('workspace')
    expect(reachableAiScope('instance', false, false)).toBe('instance')
  })

  it('falls back from library to workspace before instance', () => {
    expect(reachableAiScope('library', true, false)).toBe('workspace')
    expect(reachableAiScope('library', false, false)).toBe('instance')
  })

  it('falls back from workspace to instance', () => {
    expect(reachableAiScope('workspace', false, true)).toBe('instance')
  })
})

describe('aiReadinessStage', () => {
  it('reports missing accounts before missing bindings', () => {
    expect(aiReadinessStage(0, false)).toBe('missingAccounts')
    expect(aiReadinessStage(0, true)).toBe('missingAccounts')
  })

  it('separates ready from missing bindings once an account exists', () => {
    expect(aiReadinessStage(1, true)).toBe('ready')
    expect(aiReadinessStage(1, false)).toBe('missingBindings')
  })
})
