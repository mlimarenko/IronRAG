import { describe, expect, it } from 'vitest';

import type {
  AIBindingAssignment,
  AICredential,
  AIModelOption,
  AIPurpose,
  AIProvider,
  AIScopeKind,
  ModelPreset,
} from '@/shared/types';
import {
  isModelAvailableForCredential,
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

function credential(overrides: Partial<AICredential> = {}): AICredential {
  return {
    id: 'credential-alpha',
    scopeKind: 'instance',
    providerId: 'provider-alpha',
    providerName: 'Provider Alpha',
    providerKind: 'alpha',
    provider: provider(),
    label: 'Credential Alpha',
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
    availableCredentialIds: ['credential-alpha'],
    ...overrides,
  };
}

function preset(overrides: Partial<ModelPreset> = {}): ModelPreset {
  return {
    id: 'preset-alpha',
    scopeKind: 'instance',
    providerId: 'provider-alpha',
    providerName: 'Provider Alpha',
    providerKind: 'alpha',
    modelCatalogId: 'model-alpha',
    modelName: 'alpha-chat',
    presetName: 'Alpha Answer',
    allowedBindingPurposes: ['query_answer'],
    createdAt: '2026-01-01T00:00:00Z',
    updatedAt: '2026-01-02T00:00:00Z',
    ...overrides,
  };
}

function binding(purpose: AIPurpose, overrides: Partial<AIBindingAssignment> = {}): AIBindingAssignment {
  return {
    id: `binding-${purpose}`,
    scopeKind: 'instance',
    purpose,
    credentialId: 'credential-alpha',
    presetId: 'preset-alpha',
    state: 'configured',
    ...overrides,
  };
}

function runtimePurposePreset(purpose: AIPurpose): ModelPreset {
  return preset({
    id: `preset-${purpose}`,
    modelCatalogId: `model-${purpose}`,
    modelName: `${purpose}-model`,
    presetName: `${purpose} preset`,
    allowedBindingPurposes: [purpose],
  });
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
    presetId: `preset-${purpose}`,
  });
}

function summaryInput(overrides: {
  selectedScope?: AIScopeKind;
  availableCredentials?: AICredential[];
  localCredentials?: AICredential[];
  availablePresets?: ModelPreset[];
  localPresets?: ModelPreset[];
  bindingsForScope?: AIBindingAssignment[];
  instanceBindings?: AIBindingAssignment[];
  workspaceBindings?: AIBindingAssignment[];
  models?: AIModelOption[];
  providers?: AIProvider[];
} = {}) {
  return {
    selectedScope: overrides.selectedScope ?? 'instance',
    availableCredentials: overrides.availableCredentials ?? [credential()],
    localCredentials: overrides.localCredentials ?? [credential()],
    availablePresets: overrides.availablePresets ?? [preset()],
    localPresets: overrides.localPresets ?? [preset()],
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
        presetId: 'preset-compile',
      })],
      instanceBindings: [binding('query_answer')],
      availablePresets: [
        preset(),
        preset({
          id: 'preset-compile',
          modelCatalogId: 'model-compile',
          modelName: 'alpha-compile',
          presetName: 'Alpha Compile',
          allowedBindingPurposes: ['query_compile'],
        }),
      ],
      localPresets: [],
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
      availablePresets: runtimePurposes.map(runtimePurposePreset),
      localPresets: runtimePurposes.map(runtimePurposePreset),
      models: runtimePurposes.map(runtimePurposeModel),
    }));

    expect(summary.totalPurposes).toBe(6);
    expect(summary.executableEffectiveBindings).toBe(6);
    expect(summary.missingPurposes).toEqual([]);
  });

  it('requires executable credential preset model pairs before marking a purpose ready', () => {
    const summary = summarizeAiReadiness(summaryInput({
      availableCredentials: [credential({ state: 'revoked' })],
      localCredentials: [credential({ state: 'revoked' })],
      bindingsForScope: [binding('query_answer')],
      instanceBindings: [binding('query_answer')],
    }));

    expect(summary.executableEffectiveBindings).toBe(0);
    expect(summary.usablePresetCount).toBe(0);
    expect(summary.missingPurposes).toContain('query_answer');
  });

  it('treats bindings with unavailable models as missing', () => {
    const summary = summarizeAiReadiness(summaryInput({
      models: [model({ availabilityState: 'unavailable' })],
    }));

    expect(summary.executableEffectiveBindings).toBe(0);
    expect(summary.usablePresetCount).toBe(0);
    expect(summary.missingPurposes).toContain('query_answer');
  });

  it('keeps unchecked credential-discovered models executable until a credential-specific check disproves them', () => {
    const summary = summarizeAiReadiness(summaryInput({
      models: [model({ availabilityState: 'unknown' })],
    }));

    expect(summary.executableEffectiveBindings).toBe(1);
    expect(summary.usablePresetCount).toBe(1);
    expect(summary.missingPurposes).not.toContain('query_answer');
  });

  it('recommends the next canonical section from missing readiness data', () => {
    expect(recommendAiConfigSection(summarizeAiReadiness(summaryInput({
      availableCredentials: [],
      localCredentials: [],
    })))).toBe('credentials');

    expect(recommendAiConfigSection(summarizeAiReadiness(summaryInput({
      models: [model({ availabilityState: 'unavailable' })],
    })))).toBe('presets');

    expect(recommendAiConfigSection(summarizeAiReadiness(summaryInput({
      bindingsForScope: [],
      instanceBindings: [],
    })))).toBe('bindings');
  });
});

describe('isModelAvailableForCredential', () => {
  it('trusts unresolved catalog availability until credential-scoped discovery returns', () => {
    expect(isModelAvailableForCredential(
      model({ availabilityState: 'unknown', availableCredentialIds: ['credential-alpha'] }),
      credential(),
      {},
    )).toBe(true);
  });

  it('trusts credential-scoped discovery when it has returned', () => {
    expect(isModelAvailableForCredential(
      model({ availabilityState: 'unknown', availableCredentialIds: ['credential-alpha'] }),
      credential(),
      { 'credential-alpha': [] },
    )).toBe(false);

    expect(isModelAvailableForCredential(
      model({ availabilityState: 'unknown', availableCredentialIds: ['credential-alpha'] }),
      credential(),
      { 'credential-alpha': [model()] },
    )).toBe(true);
  });
});

describe('suggestBindingSelection', () => {
  it('prefills a compatible active credential and preset for the selected purpose', () => {
    const suggestion = suggestBindingSelection({
      purpose: 'query_answer',
      availableCredentials: [
        credential({
          id: 'credential-revoked',
          state: 'revoked',
          updatedAt: '2026-01-03T00:00:00Z',
        }),
        credential(),
      ],
      availablePresets: [
        preset({
          id: 'preset-graph',
          allowedBindingPurposes: ['extract_graph'],
        }),
        preset(),
      ],
      modelById: new Map([['model-alpha', model()]]),
    });

    expect(suggestion).toEqual({
      credentialId: 'credential-alpha',
      presetId: 'preset-alpha',
    });
  });

  it('keeps an existing compatible local selection when reopening an editor', () => {
    const suggestion = suggestBindingSelection({
      purpose: 'query_answer',
      availableCredentials: [credential(), credential({
        id: 'credential-beta',
        providerId: 'provider-beta',
        providerName: 'Provider Beta',
        providerKind: 'beta',
      })],
      availablePresets: [preset(), preset({
        id: 'preset-beta',
        providerId: 'provider-beta',
        providerName: 'Provider Beta',
        providerKind: 'beta',
        modelCatalogId: 'model-beta',
        modelName: 'beta-chat',
      })],
      modelById: new Map([
        ['model-alpha', model()],
        ['model-beta', model({
          id: 'model-beta',
          providerCatalogId: 'provider-beta',
          modelName: 'beta-chat',
          availableCredentialIds: ['credential-beta'],
        })],
      ]),
      preferredCredentialId: 'credential-beta',
      preferredPresetId: 'preset-beta',
    });

    expect(suggestion).toEqual({
      credentialId: 'credential-beta',
      presetId: 'preset-beta',
    });
  });

  it('leaves binding selectors empty when there is no active executable pair', () => {
    const suggestion = suggestBindingSelection({
      purpose: 'query_answer',
      availableCredentials: [credential({ state: 'revoked' })],
      availablePresets: [preset()],
      modelById: new Map([['model-alpha', model()]]),
    });

    expect(suggestion).toEqual({
      credentialId: '',
      presetId: '',
    });
  });
});
