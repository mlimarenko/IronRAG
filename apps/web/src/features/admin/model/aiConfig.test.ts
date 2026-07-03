import { describe, expect, it } from 'vitest';

import type {
  AIAccount,
  AIBindingAssignment,
  AIModelOption,
  AIPurpose,
  AIProvider,
  AIScopeKind,
} from '@/shared/types';
import {
  isModelAvailableForAccount,
  recommendAiConfigSection,
  summarizeAiReadiness,
  suggestBindingSelection,
} from './aiConfig';

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
    capabilities: {},
    runtime: {},
    uiHints: {},
    modelCount: 1,
    credentialCount: 1,
    ...overrides,
  };
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
  };
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
  };
}

function binding(purpose: AIPurpose, overrides: Partial<AIBindingAssignment> = {}): AIBindingAssignment {
  return {
    id: `binding-${purpose}`,
    scopeKind: 'instance',
    purpose,
    accountId: 'account-alpha',
    modelCatalogId: 'model-alpha',
    state: 'configured',
    ...overrides,
  };
}

function runtimePurposeModel(purpose: AIPurpose): AIModelOption {
  return model({
    id: `model-${purpose}`,
    modelName: `${purpose}-model`,
    allowedBindingPurposes: [purpose],
  });
}

function runtimePurposeBinding(purpose: AIPurpose): AIBindingAssignment {
  return binding(purpose, {
    id: `binding-${purpose}`,
    purpose,
    modelCatalogId: `model-${purpose}`,
  });
}

function summaryInput(overrides: {
  selectedScope?: AIScopeKind;
  availableAccounts?: AIAccount[];
  localAccounts?: AIAccount[];
  bindingsForScope?: AIBindingAssignment[];
  instanceBindings?: AIBindingAssignment[];
  workspaceBindings?: AIBindingAssignment[];
  models?: AIModelOption[];
  providers?: AIProvider[];
} = {}) {
  return {
    selectedScope: overrides.selectedScope ?? 'instance',
    availableAccounts: overrides.availableAccounts ?? [account()],
    localAccounts: overrides.localAccounts ?? [account()],
    bindingsForScope: overrides.bindingsForScope ?? [binding('query_answer')],
    instanceBindings: overrides.instanceBindings ?? [binding('query_answer')],
    workspaceBindings: overrides.workspaceBindings ?? [],
    models: overrides.models ?? [model()],
    providers: overrides.providers ?? [provider()],
  };
}

describe('summarizeAiReadiness', () => {
  it('counts effective inherited runtime bindings for child scopes', () => {
    const summary = summarizeAiReadiness(summaryInput({
      selectedScope: 'library',
      bindingsForScope: [],
      workspaceBindings: [binding('query_compile', {
        id: 'binding-workspace-query-compile',
        scopeKind: 'workspace',
        modelCatalogId: 'model-compile',
      })],
      instanceBindings: [binding('query_answer')],
      models: [
        model(),
        model({
          id: 'model-compile',
          modelName: 'alpha-compile',
          allowedBindingPurposes: ['query_compile'],
        }),
      ],
    }));

    expect(summary.executableEffectiveBindings).toBe(2);
    expect(summary.localBindingCount).toBe(0);
    expect(summary.missingPurposes).toContain('embed_chunk');
    expect(summary.missingPurposes).not.toContain('query_answer');
    expect(summary.missingPurposes).not.toContain('query_compile');
  });

  it('requires query retrieval as an executable runtime binding', () => {
    const runtimePurposes: AIPurpose[] = [
      'extract_graph',
      'embed_chunk',
      'query_retrieve',
      'query_compile',
      'query_answer',
      'agent',
    ];
    const summary = summarizeAiReadiness(summaryInput({
      bindingsForScope: runtimePurposes.map(runtimePurposeBinding),
      instanceBindings: runtimePurposes.map(runtimePurposeBinding),
      models: runtimePurposes.map(runtimePurposeModel),
    }));

    expect(summary.totalPurposes).toBe(6);
    expect(summary.executableEffectiveBindings).toBe(6);
    expect(summary.missingPurposes).toEqual([]);
  });

  it('requires an executable account/model pair before marking a purpose ready', () => {
    const summary = summarizeAiReadiness(summaryInput({
      availableAccounts: [account({ state: 'revoked' })],
      localAccounts: [account({ state: 'revoked' })],
      bindingsForScope: [binding('query_answer')],
      instanceBindings: [binding('query_answer')],
    }));

    expect(summary.executableEffectiveBindings).toBe(0);
    expect(summary.missingPurposes).toContain('query_answer');
  });

  it('treats bindings with unavailable models as missing', () => {
    const summary = summarizeAiReadiness(summaryInput({
      models: [model({ availabilityState: 'unavailable' })],
    }));

    expect(summary.executableEffectiveBindings).toBe(0);
    expect(summary.missingPurposes).toContain('query_answer');
  });

  it('keeps unchecked account-discovered models executable until an account-specific check disproves them', () => {
    const summary = summarizeAiReadiness(summaryInput({
      models: [model({ availabilityState: 'unknown' })],
    }));

    expect(summary.executableEffectiveBindings).toBe(1);
    expect(summary.missingPurposes).not.toContain('query_answer');
  });

  it('recommends the next canonical section from missing readiness data', () => {
    expect(recommendAiConfigSection(summarizeAiReadiness(summaryInput({
      availableAccounts: [],
      localAccounts: [],
    })))).toBe('accounts');

    expect(recommendAiConfigSection(summarizeAiReadiness(summaryInput({
      bindingsForScope: [],
      instanceBindings: [],
    })))).toBe('bindings');
  });
});

describe('isModelAvailableForAccount', () => {
  it('trusts unresolved catalog availability until account-scoped discovery returns', () => {
    expect(isModelAvailableForAccount(
      model({ availabilityState: 'unknown', availableAccountIds: ['account-alpha'] }),
      account(),
      {},
    )).toBe(true);
  });

  it('trusts account-scoped discovery when it has returned', () => {
    expect(isModelAvailableForAccount(
      model({ availabilityState: 'unknown', availableAccountIds: ['account-alpha'] }),
      account(),
      { 'account-alpha': [] },
    )).toBe(false);

    expect(isModelAvailableForAccount(
      model({ availabilityState: 'unknown', availableAccountIds: ['account-alpha'] }),
      account(),
      { 'account-alpha': [model()] },
    )).toBe(true);
  });
});

describe('suggestBindingSelection', () => {
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
    });

    expect(suggestion).toEqual({
      accountId: 'account-alpha',
      modelCatalogId: 'model-alpha',
    });
  });

  it('keeps an existing compatible local selection when reopening an editor', () => {
    const suggestion = suggestBindingSelection({
      purpose: 'query_answer',
      availableAccounts: [account(), account({
        id: 'account-beta',
        providerId: 'provider-beta',
        providerName: 'Provider Beta',
        providerKind: 'beta',
      })],
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
    });

    expect(suggestion).toEqual({
      accountId: 'account-beta',
      modelCatalogId: 'model-beta',
    });
  });

  it('leaves binding selectors empty when there is no active executable pair', () => {
    const suggestion = suggestBindingSelection({
      purpose: 'query_answer',
      availableAccounts: [account({ state: 'revoked' })],
      models: [model()],
    });

    expect(suggestion).toEqual({
      accountId: '',
      modelCatalogId: '',
    });
  });
});
