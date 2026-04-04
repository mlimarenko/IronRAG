import { describe, expect, it } from 'vitest'

import type { BootstrapAiSetupDescriptor } from 'src/models/ui/auth'

import {
  buildBootstrapSetupAiPayload,
  createEmptyBindingDraft,
  defaultBindingInput,
  envConfiguredProviders,
  isAiSetupReady,
  missingCredentialProviders,
  PURPOSE_ORDER,
  providersForPurpose,
  selectedProviderDescriptors,
  syncBindingInput,
} from './bootstrapSetupForm'

const TEST_DEEPSEEK_API_KEY = 'test-deepseek-token'

function sampleAiSetup(): BootstrapAiSetupDescriptor {
  return {
    providers: [
      {
        providerCatalogId: 'provider-openai',
        providerKind: 'openai',
        displayName: 'OpenAI',
        apiStyle: 'openai_compatible',
        lifecycleState: 'active',
        credentialSource: 'env',
      },
      {
        providerCatalogId: 'provider-deepseek',
        providerKind: 'deepseek',
        displayName: 'DeepSeek',
        apiStyle: 'openai_compatible',
        lifecycleState: 'active',
        credentialSource: 'missing',
      },
    ],
    models: [
      {
        id: 'model-deepseek-chat',
        providerCatalogId: 'provider-deepseek',
        modelName: 'deepseek-chat',
        capabilityKind: 'chat',
        modalityKind: 'text',
        allowedBindingPurposes: ['extract_graph'],
        contextWindow: null,
        maxOutputTokens: null,
      },
      {
        id: 'model-openai-extract',
        providerCatalogId: 'provider-openai',
        modelName: 'gpt-5.4',
        capabilityKind: 'chat',
        modalityKind: 'multimodal',
        allowedBindingPurposes: ['extract_graph', 'query_answer'],
        contextWindow: null,
        maxOutputTokens: null,
      },
      {
        id: 'model-openai-embed',
        providerCatalogId: 'provider-openai',
        modelName: 'text-embedding-3-large',
        capabilityKind: 'embedding',
        modalityKind: 'text',
        allowedBindingPurposes: ['embed_chunk'],
        contextWindow: null,
        maxOutputTokens: null,
      },
      {
        id: 'model-openai-answer',
        providerCatalogId: 'provider-openai',
        modelName: 'gpt-5.4',
        capabilityKind: 'chat',
        modalityKind: 'multimodal',
        allowedBindingPurposes: ['query_answer'],
        contextWindow: null,
        maxOutputTokens: null,
      },
      {
        id: 'model-openai-vision',
        providerCatalogId: 'provider-openai',
        modelName: 'gpt-5.4-mini',
        capabilityKind: 'chat',
        modalityKind: 'multimodal',
        allowedBindingPurposes: ['vision'],
        contextWindow: null,
        maxOutputTokens: null,
      },
    ],
    bindingSelections: [
      {
        bindingPurpose: 'extract_graph',
        providerKind: 'deepseek',
        modelCatalogId: 'model-deepseek-chat',
        configured: true,
      },
      {
        bindingPurpose: 'embed_chunk',
        providerKind: 'openai',
        modelCatalogId: 'model-openai-embed',
        configured: true,
      },
      {
        bindingPurpose: 'query_answer',
        providerKind: 'openai',
        modelCatalogId: 'model-openai-answer',
        configured: true,
      },
      {
        bindingPurpose: 'vision',
        providerKind: 'openai',
        modelCatalogId: 'model-openai-vision',
        configured: true,
      },
    ],
  }
}

describe('Bootstrap setup form helpers', () => {
  it('hydrates canonical runtime bindings from the bootstrap descriptor', () => {
    const aiSetup = sampleAiSetup()
    const bindingDraft = createEmptyBindingDraft()

    for (const purpose of PURPOSE_ORDER) {
      bindingDraft[purpose] = defaultBindingInput(aiSetup, purpose)
    }

    expect(bindingDraft.extract_graph).toEqual({
      bindingPurpose: 'extract_graph',
      providerKind: 'deepseek',
      modelCatalogId: 'model-deepseek-chat',
    })
    expect(bindingDraft.embed_chunk.providerKind).toBe('openai')
    expect(bindingDraft.query_answer.modelCatalogId).toBe('model-openai-answer')
    expect(bindingDraft.vision.modelCatalogId).toBe('model-openai-vision')
  })

  it('deduplicates selected providers and only requires missing credentials', () => {
    const aiSetup = sampleAiSetup()
    const bindingDraft = createEmptyBindingDraft()

    for (const purpose of PURPOSE_ORDER) {
      bindingDraft[purpose] = defaultBindingInput(aiSetup, purpose)
    }

    expect(
      selectedProviderDescriptors(aiSetup, bindingDraft).map((provider) => provider.providerKind),
    ).toEqual(['deepseek', 'openai'])
    expect(
      envConfiguredProviders(aiSetup, bindingDraft).map((provider) => provider.providerKind),
    ).toEqual(['openai'])
    expect(
      missingCredentialProviders(aiSetup, bindingDraft).map((provider) => provider.providerKind),
    ).toEqual(['deepseek'])

    expect(isAiSetupReady(aiSetup, bindingDraft, {})).toBe(false)
    expect(isAiSetupReady(aiSetup, bindingDraft, { deepseek: TEST_DEEPSEEK_API_KEY })).toBe(true)
  })

  it('keeps OpenAI available for graph extraction when the catalog exposes it', () => {
    const aiSetup = sampleAiSetup()

    const extractProviders = providersForPurpose(aiSetup, 'extract_graph').map(
      (provider) => provider.providerKind,
    )

    expect(extractProviders).toEqual(['openai', 'deepseek'])
  })

  it('builds one canonical bootstrap payload and keeps env-backed providers tokenless', () => {
    const aiSetup = sampleAiSetup()
    const bindingDraft = createEmptyBindingDraft()

    for (const purpose of PURPOSE_ORDER) {
      bindingDraft[purpose] = defaultBindingInput(aiSetup, purpose)
    }

    const payload = buildBootstrapSetupAiPayload(aiSetup, bindingDraft, {
      deepseek: TEST_DEEPSEEK_API_KEY,
    })

    expect(payload).not.toBeNull()
    expect(payload?.credentials).toEqual([
      {
        providerKind: 'deepseek',
        apiKey: TEST_DEEPSEEK_API_KEY,
      },
      {
        providerKind: 'openai',
        apiKey: null,
      },
    ])
    expect(payload?.bindingSelections).toEqual([
      {
        bindingPurpose: 'extract_graph',
        providerKind: 'deepseek',
        modelCatalogId: 'model-deepseek-chat',
      },
      {
        bindingPurpose: 'embed_chunk',
        providerKind: 'openai',
        modelCatalogId: 'model-openai-embed',
      },
      {
        bindingPurpose: 'query_answer',
        providerKind: 'openai',
        modelCatalogId: 'model-openai-answer',
      },
      {
        bindingPurpose: 'vision',
        providerKind: 'openai',
        modelCatalogId: 'model-openai-vision',
      },
    ])
  })

  it('resets the selected model when the provider changes', () => {
    const aiSetup = sampleAiSetup()
    const next = syncBindingInput(aiSetup, 'extract_graph', {
      bindingPurpose: 'extract_graph',
      providerKind: 'openai',
      modelCatalogId: 'model-openai-answer',
    })

    expect(next).toEqual({
      bindingPurpose: 'extract_graph',
      providerKind: 'openai',
      modelCatalogId: 'model-openai-extract',
    })
  })
})
