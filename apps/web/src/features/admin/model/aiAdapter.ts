import type {
  AiBindingAssignmentResponse,
  AiScopeKind,
  ModelCatalogEntryResponse,
  ModelPresetResponse,
  ProviderCatalogEntryResponse,
  ProviderCredentialResponse,
} from '@/shared/api/generated';
import type {
  AIBindingAssignment,
  AICredential,
  AIModelOption,
  AIProvider,
  ModelPreset,
} from '@/shared/types/index';
import {
  hasStoredApiKeySummary,
  resolveProviderBaseUrlPolicy,
  resolveProviderCredentialPolicy,
  resolveProviderModelDiscovery,
} from '@/shared/lib/ai-provider';

function normalizeArray<T>(value: T[] | null | undefined): T[] {
  return Array.isArray(value) ? value : [];
}

function scopeKind(value: AiScopeKind) {
  if (value === 'instance' || value === 'workspace' || value === 'library') {
    return value;
  }
  throw new Error(`AI response has invalid scopeKind: ${value}`);
}

function optionalString(value: string | null | undefined) {
  return value ?? undefined;
}

function optionalNumber(value: number | null | undefined) {
  return value ?? undefined;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value && typeof value === 'object' && !Array.isArray(value));
}

function parseExtraParams(value: unknown) {
  if (!value) {
    return undefined;
  }
  if (isRecord(value)) {
    return value;
  }
  return undefined;
}

function mapProvider(raw: ProviderCatalogEntryResponse): AIProvider {
  const credentialPolicy = resolveProviderCredentialPolicy(raw);
  const baseUrlPolicy = resolveProviderBaseUrlPolicy(raw);
  const modelDiscovery = resolveProviderModelDiscovery(raw);

  return {
    id: raw.id,
    displayName: raw.displayName,
    kind: raw.providerKind,
    apiStyle: raw.apiStyle,
    lifecycleState:
      raw.lifecycleState === 'active'
        ? 'active'
        : raw.lifecycleState === 'deprecated'
          ? 'deprecated'
          : 'preview',
    ...(raw.defaultBaseUrl ? { defaultBaseUrl: raw.defaultBaseUrl } : {}),
    apiKeyRequired: credentialPolicy.apiKeyRequired,
    baseUrlRequired: credentialPolicy.baseUrlRequired,
    credentialPolicy,
    baseUrlPolicy,
    modelDiscovery,
    capabilities: raw.capabilities,
    runtime: raw.runtime,
    uiHints: raw.uiHints && typeof raw.uiHints === 'object' && !Array.isArray(raw.uiHints)
      ? raw.uiHints as Record<string, unknown>
      : {},
    modelCount: 0,
    credentialCount: 0,
  };
}

function mapCredential(
  raw: ProviderCredentialResponse,
  providers: AIProvider[],
): AICredential {
  const provider =
    providers.find((entry) => entry.id === raw.providerCatalogId) ??
    { displayName: 'Unknown', kind: 'unknown' };
  return {
    id: raw.id,
    scopeKind: scopeKind(raw.scopeKind),
    ...(raw.workspaceId ? { workspaceId: raw.workspaceId } : {}),
    ...(raw.libraryId ? { libraryId: raw.libraryId } : {}),
    providerId: raw.providerCatalogId ?? '',
    providerName: provider.displayName,
    providerKind: provider.kind,
    ...('id' in provider ? { provider } : {}),
    label: raw.label ?? '',
    state:
      raw.credentialState === 'active' ||
      raw.credentialState === 'invalid' ||
      raw.credentialState === 'revoked'
        ? raw.credentialState
        : 'unchecked',
    createdAt: raw.createdAt ?? '',
    updatedAt: raw.updatedAt ?? '',
    ...(raw.baseUrl ? { baseUrl: raw.baseUrl } : {}),
    apiKeySummary: hasStoredApiKeySummary(raw.apiKeySummary) ? (raw.apiKeySummary ?? '') : '',
  };
}

function mapModelOption(raw: ModelCatalogEntryResponse): AIModelOption {
  return {
    id: raw.id,
    providerCatalogId: raw.providerCatalogId,
    modelName: raw.modelName,
    capabilityKind: raw.capabilityKind ?? '',
    modalityKind: raw.modalityKind ?? '',
    allowedBindingPurposes: raw.allowedBindingPurposes,
    ...(optionalNumber(raw.contextWindow) !== undefined ? { contextWindow: raw.contextWindow as number } : {}),
    ...(optionalNumber(raw.maxOutputTokens) !== undefined ? { maxOutputTokens: raw.maxOutputTokens as number } : {}),
    availabilityState: raw.availabilityState,
    availableCredentialIds: raw.availableCredentialIds,
  };
}

function mapPreset(
  raw: ModelPresetResponse,
  providers: AIProvider[],
  models: AIModelOption[],
): ModelPreset {
  const model = models.find((entry) => entry.id === raw.modelCatalogId);
  const provider = model
    ? (providers.find((entry) => entry.id === model.providerCatalogId) ??
      { displayName: 'Unknown', kind: 'unknown' })
    : { displayName: 'Unknown', kind: 'unknown' };
  const extraParams = parseExtraParams(raw.extraParametersJson);
  return {
    id: raw.id,
    scopeKind: scopeKind(raw.scopeKind),
    ...(raw.workspaceId ? { workspaceId: raw.workspaceId } : {}),
    ...(raw.libraryId ? { libraryId: raw.libraryId } : {}),
    providerId: model?.providerCatalogId ?? '',
    providerName: provider.displayName,
    providerKind: provider.kind,
    modelCatalogId: raw.modelCatalogId ?? '',
    modelName: model?.modelName ?? raw.modelCatalogId ?? '',
    presetName: raw.presetName ?? '',
    allowedBindingPurposes: model?.allowedBindingPurposes ?? [],
    ...(optionalString(raw.systemPrompt) ? { systemPrompt: raw.systemPrompt as string } : {}),
    ...(optionalNumber(raw.temperature) !== undefined ? { temperature: raw.temperature as number } : {}),
    ...(optionalNumber(raw.topP) !== undefined ? { topP: raw.topP as number } : {}),
    ...(optionalNumber(raw.maxOutputTokensOverride) !== undefined ? { maxOutputTokens: raw.maxOutputTokensOverride as number } : {}),
    ...(extraParams !== undefined ? { extraParams } : {}),
    createdAt: raw.createdAt ?? '',
    updatedAt: raw.updatedAt ?? '',
  };
}

function mapBinding(raw: AiBindingAssignmentResponse): AIBindingAssignment {
  return {
    id: raw.id,
    scopeKind: scopeKind(raw.scopeKind),
    ...(raw.workspaceId ? { workspaceId: raw.workspaceId } : {}),
    ...(raw.libraryId ? { libraryId: raw.libraryId } : {}),
    purpose: raw.bindingPurpose,
    credentialId: raw.providerCredentialId,
    presetId: raw.modelPresetId,
    state:
      raw.bindingState === 'invalid'
        ? 'invalid'
        : raw.bindingState === 'inactive'
          ? 'inactive'
          : 'configured',
  };
}

export function mapProviderList(raw: ProviderCatalogEntryResponse[] | null | undefined): AIProvider[] {
  return normalizeArray(raw).map(mapProvider);
}

export function mapModelList(raw: ModelCatalogEntryResponse[] | null | undefined): AIModelOption[] {
  return normalizeArray(raw).map(mapModelOption);
}

export function mapCredentialList(
  raw: ProviderCredentialResponse[] | null | undefined,
  providers: AIProvider[],
): AICredential[] {
  return normalizeArray(raw).map((entry) => mapCredential(entry, providers));
}

export function mapPresetList(
  raw: ModelPresetResponse[] | null | undefined,
  providers: AIProvider[],
  models: AIModelOption[],
): ModelPreset[] {
  return normalizeArray(raw).map((entry) => mapPreset(entry, providers, models));
}

export function mapBindingList(
  raw: AiBindingAssignmentResponse[] | null | undefined,
): AIBindingAssignment[] {
  return normalizeArray(raw).map(mapBinding);
}
