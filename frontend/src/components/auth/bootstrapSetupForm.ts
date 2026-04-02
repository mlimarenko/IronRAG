import type {
  BootstrapAiSetupDescriptor,
  BootstrapBindingInput,
  BootstrapBindingPurpose,
  BootstrapCredentialInput,
  BootstrapProviderDescriptor,
  BootstrapSetupAiPayload,
} from 'src/models/ui/auth'

export const PURPOSE_ORDER: BootstrapBindingPurpose[] = [
  'extract_graph',
  'embed_chunk',
  'query_answer',
  'vision',
]

export type BootstrapBindingDraft = Record<BootstrapBindingPurpose, BootstrapBindingInput>

export function createEmptyBindingDraft(): BootstrapBindingDraft {
  return {
    extract_graph: {
      bindingPurpose: 'extract_graph',
      providerKind: '',
      modelCatalogId: '',
    },
    embed_chunk: {
      bindingPurpose: 'embed_chunk',
      providerKind: '',
      modelCatalogId: '',
    },
    query_answer: {
      bindingPurpose: 'query_answer',
      providerKind: '',
      modelCatalogId: '',
    },
    vision: {
      bindingPurpose: 'vision',
      providerKind: '',
      modelCatalogId: '',
    },
  }
}

function providerSortValue(
  aiSetup: BootstrapAiSetupDescriptor | null,
  providerKind: string,
): string {
  const provider = aiSetup?.providers.find((entry) => entry.providerKind === providerKind)
  if (!provider) {
    return providerKind
  }
  const credentialRank = provider.credentialSource === 'env' ? '0' : '1'
  return `${credentialRank}:${provider.displayName}:${provider.providerKind}`
}

export function providersForPurpose(
  aiSetup: BootstrapAiSetupDescriptor | null,
  purpose: BootstrapBindingPurpose,
): BootstrapProviderDescriptor[] {
  if (!aiSetup) {
    return []
  }
  const allowedProviderCatalogIds = new Set(
    aiSetup.models
      .filter((model) => model.allowedBindingPurposes.includes(purpose))
      .map((model) => model.providerCatalogId),
  )
  return aiSetup.providers
    .filter((provider) => allowedProviderCatalogIds.has(provider.providerCatalogId))
    .sort((left, right) =>
      providerSortValue(aiSetup, left.providerKind).localeCompare(
        providerSortValue(aiSetup, right.providerKind),
      ),
    )
}

export function modelsForPurpose(
  aiSetup: BootstrapAiSetupDescriptor | null,
  purpose: BootstrapBindingPurpose,
  providerKind: string,
) {
  if (!aiSetup) {
    return []
  }
  const provider = aiSetup.providers.find((entry) => entry.providerKind === providerKind)
  if (!provider) {
    return []
  }
  return aiSetup.models
    .filter(
      (model) =>
        model.providerCatalogId === provider.providerCatalogId &&
        model.allowedBindingPurposes.includes(purpose),
    )
    .sort((left, right) => left.modelName.localeCompare(right.modelName))
}

export function defaultBindingInput(
  aiSetup: BootstrapAiSetupDescriptor | null,
  purpose: BootstrapBindingPurpose,
): BootstrapBindingInput {
  const providers = providersForPurpose(aiSetup, purpose)
  const configuredSelection = aiSetup?.bindingSelections.find(
    (selection) => selection.bindingPurpose === purpose,
  )
  const configuredProviderKind =
    configuredSelection?.providerKind &&
    providers.some((provider) => provider.providerKind === configuredSelection.providerKind)
      ? configuredSelection.providerKind
      : (providers[0]?.providerKind ?? '')
  const models = modelsForPurpose(aiSetup, purpose, configuredProviderKind)
  const configuredModelId =
    configuredSelection?.modelCatalogId &&
    models.some((model) => model.id === configuredSelection.modelCatalogId)
      ? configuredSelection.modelCatalogId
      : (models[0]?.id ?? '')

  return {
    bindingPurpose: purpose,
    providerKind: configuredProviderKind,
    modelCatalogId: configuredModelId,
  }
}

export function syncBindingInput(
  aiSetup: BootstrapAiSetupDescriptor | null,
  purpose: BootstrapBindingPurpose,
  current: BootstrapBindingInput,
): BootstrapBindingInput {
  const providers = providersForPurpose(aiSetup, purpose)
  const nextProviderKind = providers.some(
    (provider) => provider.providerKind === current.providerKind,
  )
    ? current.providerKind
    : (providers[0]?.providerKind ?? '')
  const models = modelsForPurpose(aiSetup, purpose, nextProviderKind)
  const nextModelCatalogId = models.some((model) => model.id === current.modelCatalogId)
    ? current.modelCatalogId
    : (models[0]?.id ?? '')

  return {
    bindingPurpose: purpose,
    providerKind: nextProviderKind,
    modelCatalogId: nextModelCatalogId,
  }
}

export function selectedProviderDescriptors(
  aiSetup: BootstrapAiSetupDescriptor | null,
  bindingDraft: BootstrapBindingDraft,
): BootstrapProviderDescriptor[] {
  if (!aiSetup) {
    return []
  }
  const seen = new Set<string>()
  return PURPOSE_ORDER.map((purpose) => bindingDraft[purpose].providerKind)
    .filter((providerKind) => {
      if (!providerKind || seen.has(providerKind)) {
        return false
      }
      seen.add(providerKind)
      return true
    })
    .map((providerKind) =>
      aiSetup.providers.find((provider) => provider.providerKind === providerKind),
    )
    .filter((provider): provider is BootstrapProviderDescriptor => provider !== undefined)
}

export function envConfiguredProviders(
  aiSetup: BootstrapAiSetupDescriptor | null,
  bindingDraft: BootstrapBindingDraft,
): BootstrapProviderDescriptor[] {
  return selectedProviderDescriptors(aiSetup, bindingDraft).filter(
    (provider) => provider.credentialSource === 'env',
  )
}

export function missingCredentialProviders(
  aiSetup: BootstrapAiSetupDescriptor | null,
  bindingDraft: BootstrapBindingDraft,
): BootstrapProviderDescriptor[] {
  return selectedProviderDescriptors(aiSetup, bindingDraft).filter(
    (provider) => provider.credentialSource === 'missing',
  )
}

export function unavailablePurposes(
  aiSetup: BootstrapAiSetupDescriptor | null,
): BootstrapBindingPurpose[] {
  return PURPOSE_ORDER.filter((purpose) => providersForPurpose(aiSetup, purpose).length === 0)
}

export function isAiSetupReady(
  aiSetup: BootstrapAiSetupDescriptor | null,
  bindingDraft: BootstrapBindingDraft,
  credentialDraft: Record<string, string>,
): boolean {
  if (!aiSetup) {
    return true
  }
  if (unavailablePurposes(aiSetup).length > 0) {
    return false
  }
  if (
    PURPOSE_ORDER.some((purpose) => {
      const binding = bindingDraft[purpose]
      return !binding.providerKind || !binding.modelCatalogId
    })
  ) {
    return false
  }
  return missingCredentialProviders(aiSetup, bindingDraft).every(
    (provider) => (credentialDraft[provider.providerKind]?.trim().length ?? 0) > 0,
  )
}

export function buildBootstrapCredentials(
  aiSetup: BootstrapAiSetupDescriptor | null,
  bindingDraft: BootstrapBindingDraft,
  credentialDraft: Record<string, string>,
): BootstrapCredentialInput[] {
  return selectedProviderDescriptors(aiSetup, bindingDraft).map((provider) => ({
    providerKind: provider.providerKind,
    apiKey:
      provider.credentialSource === 'env'
        ? null
        : credentialDraft[provider.providerKind]?.trim() || null,
  }))
}

export function buildBootstrapSetupAiPayload(
  aiSetup: BootstrapAiSetupDescriptor | null,
  bindingDraft: BootstrapBindingDraft,
  credentialDraft: Record<string, string>,
): BootstrapSetupAiPayload | null {
  if (!aiSetup) {
    return null
  }
  return {
    credentials: buildBootstrapCredentials(aiSetup, bindingDraft, credentialDraft),
    bindingSelections: PURPOSE_ORDER.map((purpose) => ({
      bindingPurpose: purpose,
      providerKind: bindingDraft[purpose].providerKind,
      modelCatalogId: bindingDraft[purpose].modelCatalogId,
    })),
  }
}
