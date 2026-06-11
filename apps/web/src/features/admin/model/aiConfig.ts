import type { TFunction } from 'i18next';
import type {
  AIBindingAssignment,
  AICredential,
  AIModelOption,
  AIProvider,
  AIPurpose,
  AIScopeKind,
  ModelPreset,
} from '@/shared/types';

export type AiConfigSection =
  | 'bindings'
  | 'credentials'
  | 'presets'
  | 'providers'
  | 'models'
  // Pricing folded into the AI → Catalog group (ADM-08/RM-07): it prices the
  // very models listed beside it, so it belongs here rather than as an
  // isolated top-level admin tab.
  | 'pricing';

export type AiConfigDataState<T> = {
  isLoading: boolean;
  error: unknown;
  data: T | undefined;
};

export type AiScopeContext = {
  workspaceId?: string | undefined;
  libraryId?: string | undefined;
};

export type AiScopeQueryParams = {
  query?: {
    scopeKind?: AIScopeKind;
    workspaceId?: string | undefined;
    libraryId?: string | undefined;
  };
};

export type LocalAiScopeQueryParams = {
  query: {
    scopeKind: AIScopeKind;
    workspaceId?: string | undefined;
    libraryId?: string | undefined;
  };
};

export type DefinedAiScopeQuery = {
  scopeKind?: AIScopeKind;
  workspaceId?: string;
  libraryId?: string;
};

export type BindingResolution = {
  localBinding: AIBindingAssignment | null;
  effectiveBinding: AIBindingAssignment | null;
  sourceKind: AIScopeKind | null;
};

export type CredentialModelLoadState = 'loading' | 'ready' | 'failed';

export type AiReadinessSummary = {
  totalPurposes: number;
  executableEffectiveBindings: number;
  localBindingCount: number;
  missingPurposes: AIPurpose[];
  /** Optional bindings that are not configured — the system falls back
   *  to local CPU processing (lower quality / higher latency). */
  missingOptionalPurposes: AIPurpose[];
  availableCredentialCount: number;
  activeCredentialCount: number;
  localCredentialCount: number;
  availablePresetCount: number;
  usablePresetCount: number;
  localPresetCount: number;
  visibleModelCount: number;
  availableModelCount: number;
  providerCatalogCount: number;
  configuredProviderCount: number;
  priceRuleCount: number;
};

export type AiBindingSuggestion = {
  credentialId: string;
  presetId: string;
};

export const AI_CONFIG_SECTIONS: AiConfigSection[] = [
  'bindings',
  'credentials',
  'presets',
  'providers',
  'models',
];

export const PURPOSE_ORDER: AIPurpose[] = [
  'extract_text',
  'extract_graph',
  'embed_chunk',
  'query_compile',
  'query_retrieve',
  'query_answer',
  'agent',
  'vision',
];

export const REQUIRED_RUNTIME_PURPOSE_ORDER: AIPurpose[] = [
  'extract_graph',
  'embed_chunk',
  'query_retrieve',
  'query_compile',
  'query_answer',
  'agent',
];

/** Optional bindings — system degrades to local CPU when missing. */
export const OPTIONAL_PURPOSES: AIPurpose[] = ['extract_text', 'vision'];

export function purposeLabel(value: AIPurpose, t: TFunction) {
  return t(`admin.aiPanel.purposeLabels.${value}`);
}

export function scopeLabel(value: AIScopeKind, t: TFunction) {
  return t(`admin.aiPanel.scopeLabels.${value}`);
}

export function credentialStateLabel(value: AICredential['state'], t: TFunction) {
  return t(`admin.aiPanel.credentialStateLabels.${value}`);
}

export function localScopeQuery(scopeKind: AIScopeKind, context: AiScopeContext): LocalAiScopeQueryParams {
  if (scopeKind === 'instance') {
    return { query: { scopeKind } };
  }
  if (scopeKind === 'workspace') {
    return { query: { scopeKind, workspaceId: context.workspaceId } };
  }
  return { query: { scopeKind, workspaceId: context.workspaceId, libraryId: context.libraryId } };
}

export function visibleScopeQuery(scopeKind: AIScopeKind, context: AiScopeContext): AiScopeQueryParams {
  if (scopeKind === 'instance') {
    return {};
  }
  if (scopeKind === 'workspace') {
    return { query: { workspaceId: context.workspaceId } };
  }
  return { query: { workspaceId: context.workspaceId, libraryId: context.libraryId } };
}

export function compactScopeQuery(params: AiScopeQueryParams['query']): DefinedAiScopeQuery {
  return {
    ...(params?.scopeKind ? { scopeKind: params.scopeKind } : {}),
    ...(params?.workspaceId ? { workspaceId: params.workspaceId } : {}),
    ...(params?.libraryId ? { libraryId: params.libraryId } : {}),
  };
}

export function modelCatalogScopeQuery(params: AiScopeQueryParams['query']) {
  return {
    ...(params?.workspaceId ? { workspaceId: params.workspaceId } : {}),
    ...(params?.libraryId ? { libraryId: params.libraryId } : {}),
  };
}

export function formatPresetLabel(preset: Pick<ModelPreset, 'presetName' | 'modelName'>) {
  const presetName = preset.presetName.trim();
  const modelName = preset.modelName.trim();
  if (!presetName) {
    return modelName;
  }
  if (!modelName) {
    return presetName;
  }
  if (presetName.toLocaleLowerCase().includes(modelName.toLocaleLowerCase())) {
    return presetName;
  }
  return `${presetName} · ${modelName}`;
}

export function badgeClass(value: 'ready' | 'warning' | 'failed') {
  return value === 'ready' ? 'status-ready' : value === 'failed' ? 'status-failed' : 'status-warning';
}

export function parseNumber(value: string): number | null {
  const normalized = value.trim();
  if (!normalized) {
    return null;
  }
  const parsed = Number(normalized);
  return Number.isFinite(parsed) ? parsed : null;
}

export function parseInteger(value: string): number | null {
  const normalized = value.trim();
  if (!normalized) {
    return null;
  }
  const parsed = Number.parseInt(normalized, 10);
  return Number.isFinite(parsed) ? parsed : null;
}

export function isModelAvailableForCredential(
  model: AIModelOption | undefined,
  credential: AICredential | null | undefined,
  modelsByCredentialId: Record<string, AIModelOption[]>,
): boolean {
  if (!model || !credential) {
    return true;
  }
  const discoveredModels = modelsByCredentialId[credential.id];
  if (!discoveredModels) {
    return model.availabilityState !== 'unavailable';
  }
  return discoveredModels.some(entry => entry.id === model.id);
}

function canUsePresetWithCredential({
  purpose,
  credential,
  preset,
  model,
  modelsByCredentialId,
}: {
  purpose: AIPurpose;
  credential: AICredential | undefined;
  preset: ModelPreset | undefined;
  model: AIModelOption | undefined;
  modelsByCredentialId: Record<string, AIModelOption[]>;
}) {
  if (!credential || credential.state !== 'active' || !preset || !model) {
    return false;
  }
  if (preset.providerId !== credential.providerId || model.providerCatalogId !== credential.providerId) {
    return false;
  }
  if (!preset.allowedBindingPurposes.includes(purpose) || !model.allowedBindingPurposes.includes(purpose)) {
    return false;
  }
  if (model.availabilityState === 'unavailable') {
    return false;
  }
  return isModelAvailableForCredential(model, credential, modelsByCredentialId);
}

export function formatModelLabel(model: AIModelOption, providers: AIProvider[]) {
  const provider = providers.find(entry => entry.id === model.providerCatalogId);
  return provider ? `${provider.displayName} · ${model.modelName}` : model.modelName;
}

export function matchesFilter(values: Array<string | undefined>, filter: string) {
  const normalized = filter.trim().toLocaleLowerCase();
  if (!normalized) {
    return true;
  }
  return values.some(value => value?.toLocaleLowerCase().includes(normalized));
}

export function compareByUpdatedAtDesc(
  left: { updatedAt: string; id: string },
  right: { updatedAt: string; id: string },
) {
  return right.updatedAt.localeCompare(left.updatedAt) || left.id.localeCompare(right.id);
}

export function resolveBindingForPurpose({
  purpose,
  selectedScope,
  bindingsForScope,
  instanceBindings,
  workspaceBindings,
}: {
  purpose: AIPurpose;
  selectedScope: AIScopeKind;
  bindingsForScope: AIBindingAssignment[];
  instanceBindings: AIBindingAssignment[];
  workspaceBindings: AIBindingAssignment[];
}): BindingResolution {
  const localBinding = bindingsForScope.find(entry => entry.purpose === purpose) ?? null;
  if (localBinding) {
    return { localBinding, effectiveBinding: localBinding, sourceKind: selectedScope };
  }
  if (selectedScope === 'library') {
    const workspaceBinding = workspaceBindings.find(entry => entry.purpose === purpose) ?? null;
    if (workspaceBinding) {
      return { localBinding: null, effectiveBinding: workspaceBinding, sourceKind: 'workspace' };
    }
  }
  const instanceBinding = instanceBindings.find(entry => entry.purpose === purpose) ?? null;
  return {
    localBinding: null,
    effectiveBinding: instanceBinding,
    sourceKind: instanceBinding ? 'instance' : null,
  };
}

export function summarizeAiReadiness({
  selectedScope,
  availableCredentials,
  localCredentials,
  availablePresets,
  localPresets,
  bindingsForScope,
  instanceBindings,
  workspaceBindings,
  models,
  providers,
  priceRuleCount,
}: {
  selectedScope: AIScopeKind;
  availableCredentials: AICredential[];
  localCredentials: AICredential[];
  availablePresets: ModelPreset[];
  localPresets: ModelPreset[];
  bindingsForScope: AIBindingAssignment[];
  instanceBindings: AIBindingAssignment[];
  workspaceBindings: AIBindingAssignment[];
  models: AIModelOption[];
  providers: AIProvider[];
  priceRuleCount?: number;
}): AiReadinessSummary {
  const credentialById = new Map(availableCredentials.map(entry => [entry.id, entry]));
  const presetById = new Map(availablePresets.map(entry => [entry.id, entry]));
  const modelById = new Map(models.map(entry => [entry.id, entry]));
  const resolutions = REQUIRED_RUNTIME_PURPOSE_ORDER.map(purpose =>
    resolveBindingForPurpose({
      purpose,
      selectedScope,
      bindingsForScope,
      instanceBindings,
      workspaceBindings,
    }),
  );
  const executablePurposeIds = new Set<AIPurpose>();
  resolutions.forEach((resolution, index) => {
    const binding = resolution.effectiveBinding;
    if (!binding || binding.state !== 'configured') {
      return;
    }
    const preset = presetById.get(binding.presetId);
    const canExecute = canUsePresetWithCredential({
      purpose: REQUIRED_RUNTIME_PURPOSE_ORDER[index],
      credential: credentialById.get(binding.credentialId),
      preset,
      model: preset ? modelById.get(preset.modelCatalogId) : undefined,
      modelsByCredentialId: {},
    });
    if (canExecute) {
      executablePurposeIds.add(REQUIRED_RUNTIME_PURPOSE_ORDER[index]);
    }
  });
  const usablePresetIds = new Set<string>();
  for (const preset of availablePresets) {
    const model = modelById.get(preset.modelCatalogId);
    const purpose = preset.allowedBindingPurposes.find(candidate =>
      availableCredentials.some(credential =>
        canUsePresetWithCredential({
          purpose: candidate,
          credential,
          preset,
          model,
          modelsByCredentialId: {},
        }),
      ),
    );
    if (purpose) {
      usablePresetIds.add(preset.id);
    }
  }
  const configuredProviderIds = new Set(availableCredentials.map(entry => entry.providerId));

  // Optional bindings — check separately from required runtime purposes.
  const missingOptionalPurposes = OPTIONAL_PURPOSES.filter(purpose => {
    const resolution = resolveBindingForPurpose({
      purpose,
      selectedScope,
      bindingsForScope,
      instanceBindings,
      workspaceBindings,
    });
    const binding = resolution.effectiveBinding;
    if (!binding || binding.state !== 'configured') return true;
    const preset = presetById.get(binding.presetId);
    return !canUsePresetWithCredential({
      purpose,
      credential: credentialById.get(binding.credentialId),
      preset,
      model: preset ? modelById.get(preset.modelCatalogId) : undefined,
      modelsByCredentialId: {},
    });
  });

  return {
    totalPurposes: REQUIRED_RUNTIME_PURPOSE_ORDER.length,
    executableEffectiveBindings: executablePurposeIds.size,
    localBindingCount: bindingsForScope.length,
    missingPurposes: REQUIRED_RUNTIME_PURPOSE_ORDER.filter(purpose => !executablePurposeIds.has(purpose)),
    missingOptionalPurposes,
    availableCredentialCount: availableCredentials.length,
    activeCredentialCount: availableCredentials.filter(entry => entry.state === 'active').length,
    localCredentialCount: localCredentials.length,
    availablePresetCount: availablePresets.length,
    usablePresetCount: usablePresetIds.size,
    localPresetCount: localPresets.length,
    visibleModelCount: models.length,
    availableModelCount: models.filter(entry => entry.availabilityState !== 'unavailable').length,
    providerCatalogCount: providers.length,
    configuredProviderCount: configuredProviderIds.size,
    priceRuleCount: priceRuleCount ?? 0,
  };
}

export function recommendAiConfigSection(summary: AiReadinessSummary): AiConfigSection {
  if (summary.activeCredentialCount === 0) {
    return 'credentials';
  }
  if (summary.usablePresetCount === 0 || summary.availableModelCount === 0) {
    return 'presets';
  }
  if (summary.missingPurposes.length > 0) {
    return 'bindings';
  }
  return 'bindings';
}

export function suggestBindingSelection({
  purpose,
  availableCredentials,
  availablePresets,
  modelById,
  preferredCredentialId,
  preferredPresetId,
}: {
  purpose: AIPurpose;
  availableCredentials: AICredential[];
  availablePresets: ModelPreset[];
  modelById: Map<string, AIModelOption>;
  preferredCredentialId?: string | undefined;
  preferredPresetId?: string | undefined;
}): AiBindingSuggestion {
  const purposePresets = availablePresets
    .filter(entry => entry.allowedBindingPurposes.includes(purpose))
    .slice()
    .sort(compareByUpdatedAtDesc);
  const activeCredentials = availableCredentials.filter(entry => entry.state === 'active');
  const preferredCredential = preferredCredentialId
    ? activeCredentials.find(entry => entry.id === preferredCredentialId)
    : undefined;
  const preferredPreset = preferredPresetId
    ? purposePresets.find(entry => entry.id === preferredPresetId)
    : undefined;
  if (canUsePresetWithCredential({
    purpose,
    credential: preferredCredential,
    preset: preferredPreset,
    model: preferredPreset ? modelById.get(preferredPreset.modelCatalogId) : undefined,
    modelsByCredentialId: {},
  })) {
    return {
      credentialId: preferredCredential.id,
      presetId: preferredPreset.id,
    };
  }

  const credentials = activeCredentials
    .slice()
    .sort((left, right) => {
      const activeDelta = Number(right.state === 'active') - Number(left.state === 'active');
      return activeDelta || compareByUpdatedAtDesc(left, right);
    });
  for (const credential of credentials) {
    const preset = purposePresets.find(entry => {
      return canUsePresetWithCredential({
        purpose,
        credential,
        preset: entry,
        model: modelById.get(entry.modelCatalogId),
        modelsByCredentialId: {},
      });
    });
    if (preset) {
      return {
        credentialId: credential.id,
        presetId: preset.id,
      };
    }
  }

  return {
    credentialId: '',
    presetId: '',
  };
}
