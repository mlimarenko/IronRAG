import { describe, expect, it } from 'vitest'

import type { AIAccount, AIBinding, AIModelOption, AIProvider, AIScopeKind } from '@/shared/types'
import type { AiBindingPurpose } from '@/shared/api/generated'
import {
  OPTIONAL_BINDING_PURPOSES,
  REQUIRED_BINDING_PURPOSES,
  isBindingExecutable,
  isProviderCredentialValidationFailure,
  isModelAvailableForAccount,
  modelSupportsBindingPurpose,
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
    capabilityKind: 'chat',
    modalityKind: 'text',
    allowedBindingPurposes: ['query_answer'],
    availabilityState: 'available',
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

  it('counts canonical binding purposes without an alias profile layer', () => {
    const runtimePurposes: AiBindingPurpose[] = [
      'extract_graph',
      'embed_chunk',
      'query_compile',
      'query_answer',
      'agent',
    ]
    const summary = summarizeAiReadiness(
      summaryInput({
        bindingsForScope: runtimePurposes.map(runtimePurposeBinding),
        instanceBindings: runtimePurposes.map(runtimePurposeBinding),
        models: REQUIRED_BINDING_PURPOSES.map((purpose) =>
          model({
            id: `model-${purpose}`,
            modelName: `${purpose}-model`,
            capabilityKind: purpose === 'embed_chunk' ? 'embedding' : 'chat',
            allowedBindingPurposes: [purpose],
          }),
        ),
      }),
    )

    expect(summary.totalPurposes).toBe(5)
    expect(summary.executableEffectiveBindings).toBe(5)
    expect(summary.missingPurposes).toEqual([])
    expect(summary.missingOptionalPurposes).toEqual(['extract_text'])
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

  it('recommends the next canonical section from missing readiness data', () => {
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

describe('canonical binding purposes', () => {
  it('uses only API purpose identifiers for required and optional bindings', () => {
    expect(REQUIRED_BINDING_PURPOSES).toEqual([
      'extract_graph',
      'embed_chunk',
      'query_compile',
      'query_answer',
      'agent',
    ])
    expect(OPTIONAL_BINDING_PURPOSES).toEqual(['extract_text'])
    expect(new Set([...REQUIRED_BINDING_PURPOSES, ...OPTIONAL_BINDING_PURPOSES]).size).toBe(6)
  })

  it('accepts models only from the selected canonical purpose', () => {
    expect(
      modelSupportsBindingPurpose(
        model({ modalityKind: 'multimodal', allowedBindingPurposes: ['extract_text'] }),
        'extract_text',
      ),
    ).toBe(true)
    expect(
      modelSupportsBindingPurpose(
        model({ modelName: 'vision-looking-name', allowedBindingPurposes: ['extract_text'] }),
        'extract_text',
      ),
    ).toBe(false)
  })

  it('requires multimodal model and provider capabilities for document understanding', () => {
    const multimodal = model({
      modalityKind: 'multimodal',
      allowedBindingPurposes: ['extract_text'],
    })

    expect(
      isBindingExecutable({
        purpose: 'extract_text',
        account: account({
          provider: provider({
            capabilities: {
              chat: 'supported',
              vision: 'supported',
            },
          }),
        }),
        model: multimodal,
        modelsByAccountId: {},
      }),
    ).toBe(true)
    expect(
      isBindingExecutable({
        purpose: 'extract_text',
        account: account({
          provider: provider({
            capabilities: {
              chat: 'supported',
              vision: 'unsupported',
            },
          }),
        }),
        model: multimodal,
        modelsByAccountId: {},
      }),
    ).toBe(false)
    expect(
      isBindingExecutable({
        purpose: 'extract_text',
        account: account({
          provider: provider({
            capabilities: {
              chat: 'supported',
              vision: 'supported',
            },
          }),
        }),
        model: model({ allowedBindingPurposes: ['extract_text'] }),
        modelsByAccountId: {},
      }),
    ).toBe(false)
  })

  it('requires explicit chat and tools support for Agent execution', () => {
    const agentModel = model({ allowedBindingPurposes: ['agent'] })

    expect(
      isBindingExecutable({
        purpose: 'agent',
        account: account(),
        model: agentModel,
        modelsByAccountId: {},
      }),
    ).toBe(true)
    expect(
      isBindingExecutable({
        purpose: 'agent',
        account: account({
          provider: provider({
            capabilities: {
              chat: 'supported',
              tools: 'unknown',
            },
          }),
        }),
        model: agentModel,
        modelsByAccountId: {},
      }),
    ).toBe(false)
    expect(
      isBindingExecutable({
        purpose: 'agent',
        account: account({
          provider: provider({
            capabilities: {
              chat: 'unknown',
              tools: 'supported',
            },
          }),
        }),
        model: agentModel,
        modelsByAccountId: {},
      }),
    ).toBe(false)
    expect(
      isBindingExecutable({
        purpose: 'agent',
        account: account(),
        model: model({
          capabilityKind: 'embedding',
          allowedBindingPurposes: ['agent'],
        }),
        modelsByAccountId: {},
      }),
    ).toBe(false)
  })

  it('keeps embed-chunk-only typed models executable for embedding', () => {
    expect(
      modelSupportsBindingPurpose(
        model({ capabilityKind: 'embedding', allowedBindingPurposes: ['embed_chunk'] }),
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
  it('does not suggest an Agent pair when provider tools support is unknown', () => {
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

    expect(suggestion).toEqual({ accountId: '', modelCatalogId: '' })
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
