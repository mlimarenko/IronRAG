import type {
  RawBindingAssignmentResponse,
  RawModelCatalogEntry,
  RawModelPresetResponse,
  RawProviderCatalogEntry,
  RawProviderCredentialResponse,
} from '@/types/api-responses';
import type {
  AIBindingAssignment,
  AICredential,
  AIModelOption,
  AIProvider,
  AIPurpose,
  ModelPreset,
} from '@/types/index';
import { hasStoredApiKeySummary } from '@/lib/ai-provider';

export function mapProvider(raw: RawProviderCatalogEntry): AIProvider {
  return {
    id: raw.id,
    displayName: raw.displayName ?? raw.providerKind ?? '',
    kind: raw.providerKind ?? 'llm',
    apiStyle: raw.apiStyle ?? '',
    lifecycleState:
      raw.lifecycleState === 'active'
        ? 'active'
        : raw.lifecycleState === 'deprecated'
          ? 'deprecated'
          : 'preview',
    defaultBaseUrl: raw.defaultBaseUrl ?? undefined,
    apiKeyRequired: raw.apiKeyRequired !== false,
    baseUrlRequired: raw.baseUrlRequired === true,
    modelCount: 0,
    credentialCount: 0,
  };
}

export function mapCredential(raw: RawProviderCredentialResponse, providers: AIProvider[]): AICredential {
  const provider = providers.find(entry => entry.id === raw.providerCatalogId) ?? { displayName: 'Unknown', kind: 'unknown' };
  return {
    id: raw.id,
    scopeKind: raw.scopeKind ?? 'workspace',
    workspaceId: raw.workspaceId ?? undefined,
    libraryId: raw.libraryId ?? undefined,
    providerId: raw.providerCatalogId ?? '',
    providerName: provider.displayName,
    providerKind: provider.kind,
    label: raw.label ?? '',
    state:
      raw.credentialState === 'active' || raw.credentialState === 'invalid' || raw.credentialState === 'revoked'
        ? raw.credentialState
        : 'unchecked',
    createdAt: raw.createdAt ?? '',
    updatedAt: raw.updatedAt ?? '',
    baseUrl: raw.baseUrl ?? undefined,
    apiKeySummary: hasStoredApiKeySummary(raw.apiKeySummary) ? (raw.apiKeySummary ?? '') : '',
  };
}

export function mapModelOption(raw: RawModelCatalogEntry): AIModelOption {
  return {
    id: raw.id,
    providerCatalogId: raw.providerCatalogId,
    modelName: raw.modelName,
    capabilityKind: raw.capabilityKind ?? '',
    modalityKind: raw.modalityKind ?? '',
    allowedBindingPurposes: (raw.allowedBindingPurposes ?? []) as AIPurpose[],
    contextWindow: raw.contextWindow ?? undefined,
    maxOutputTokens: raw.maxOutputTokens ?? undefined,
    availabilityState: raw.availabilityState ?? 'available',
    availableCredentialIds: raw.availableCredentialIds ?? [],
  };
}

export function mapPreset(raw: RawModelPresetResponse, providers: AIProvider[], models: AIModelOption[]): ModelPreset {
  const model = models.find(entry => entry.id === raw.modelCatalogId);
  const provider = model
    ? (providers.find(entry => entry.id === model.providerCatalogId) ?? { displayName: 'Unknown', kind: 'unknown' })
    : { displayName: 'Unknown', kind: 'unknown' };
  return {
    id: raw.id,
    scopeKind: raw.scopeKind ?? 'workspace',
    workspaceId: raw.workspaceId ?? undefined,
    libraryId: raw.libraryId ?? undefined,
    providerId: model?.providerCatalogId ?? '',
    providerName: provider.displayName,
    providerKind: provider.kind,
    modelCatalogId: raw.modelCatalogId ?? '',
    modelName: model?.modelName ?? raw.modelCatalogId ?? '',
    presetName: raw.presetName ?? '',
    allowedBindingPurposes: (model?.allowedBindingPurposes ?? []) as AIPurpose[],
    systemPrompt: raw.systemPrompt ?? undefined,
    temperature: raw.temperature ?? undefined,
    topP: raw.topP ?? undefined,
    maxOutputTokens: raw.maxOutputTokensOverride ?? undefined,
    extraParams: raw.extraParametersJson ?? undefined,
    createdAt: raw.createdAt ?? '',
    updatedAt: raw.updatedAt ?? '',
  };
}

export function mapBinding(raw: RawBindingAssignmentResponse): AIBindingAssignment {
  return {
    id: raw.id,
    scopeKind: raw.scopeKind ?? 'workspace',
    workspaceId: raw.workspaceId ?? undefined,
    libraryId: raw.libraryId ?? undefined,
    purpose: raw.bindingPurpose as AIPurpose,
    credentialId: raw.providerCredentialId,
    presetId: raw.modelPresetId,
    state: raw.bindingState === 'invalid' ? 'invalid' : raw.bindingState === 'inactive' ? 'inactive' : 'configured',
  };
}
