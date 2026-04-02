import { describe, expect, it } from 'vitest'

import type { BootstrapAiSetupDescriptor } from 'src/models/ui/auth'

import {
  buildBootstrapSetupAiPayload,
  createEmptyBindingDraft,
  defaultBindingInput,
  unavailablePurposes,
} from 'src/components/auth/bootstrapSetupForm'

function incompleteAiSetup(): BootstrapAiSetupDescriptor {
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
    ],
    models: [
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
    ],
    bindingSelections: [
      {
        bindingPurpose: 'query_answer',
        providerKind: 'openai',
        modelCatalogId: 'model-openai-answer',
        configured: true,
      },
    ],
  }
}

describe('Bootstrap setup fallback guards', () => {
  it('marks unavailable canonical purposes when the catalog cannot cover the full runtime profile', () => {
    const aiSetup = incompleteAiSetup()

    expect(unavailablePurposes(aiSetup)).toEqual(['extract_graph', 'embed_chunk', 'vision'])
  })

  it('emits null AI setup when the login screen has no bootstrap descriptor', () => {
    const bindingDraft = createEmptyBindingDraft()

    expect(buildBootstrapSetupAiPayload(null, bindingDraft, {})).toBeNull()
  })

  it('falls back to the first available provider/model when a configured selection is invalid', () => {
    const aiSetup: BootstrapAiSetupDescriptor = {
      ...incompleteAiSetup(),
      providers: [
        {
          providerCatalogId: 'provider-openai',
          providerKind: 'openai',
          displayName: 'OpenAI',
          apiStyle: 'openai_compatible',
          lifecycleState: 'active',
          credentialSource: 'env',
        },
      ],
      models: [
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
      ],
      bindingSelections: [
        {
          bindingPurpose: 'query_answer',
          providerKind: 'deepseek',
          modelCatalogId: 'missing-model',
          configured: true,
        },
      ],
    }

    expect(defaultBindingInput(aiSetup, 'query_answer')).toEqual({
      bindingPurpose: 'query_answer',
      providerKind: 'openai',
      modelCatalogId: 'model-openai-answer',
    })
  })
})
