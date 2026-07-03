import type { TFunction } from 'i18next';
import { z } from 'zod';

import { nonEmptyString, optionalIntegerString, optionalNumberString } from '@/shared/forms';
import type {
  AIAccount,
  AIBindingAssignment,
  AIModelOption,
  AIProvider,
  AIPurpose,
  AIScopeKind,
  PricingRule,
} from '@/shared/types';

export type AiConfigSection = 'bindings' | 'accounts' | 'catalog';

export type AiCatalogTab = 'providers' | 'models';

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

export type AccountModelLoadState = 'loading' | 'ready' | 'failed';

export type AiReadinessSummary = {
  totalPurposes: number;
  executableEffectiveBindings: number;
  localBindingCount: number;
  missingPurposes: AIPurpose[];
  /** Optional bindings that are not configured — the system falls back
   *  to local CPU processing (lower quality / higher latency). */
  missingOptionalPurposes: AIPurpose[];
  totalAccountCount: number;
  activeAccountCount: number;
  localAccountCount: number;
  visibleModelCount: number;
  availableModelCount: number;
  providerCatalogCount: number;
  configuredProviderCount: number;
  priceRuleCount: number;
};

export type AiBindingSuggestion = {
  accountId: string;
  modelCatalogId: string;
};

export const AI_CONFIG_SECTIONS: AiConfigSection[] = ['bindings', 'accounts', 'catalog'];

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

export function accountStateLabel(value: AIAccount['state'], t: TFunction) {
  return t(`admin.aiPanel.accountStateLabels.${value}`);
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

export function isModelAvailableForAccount(
  model: AIModelOption | undefined,
  account: AIAccount | null | undefined,
  modelsByAccountId: Record<string, AIModelOption[]>,
): boolean {
  if (!model || !account) {
    return true;
  }
  const discoveredModels = modelsByAccountId[account.id];
  if (!discoveredModels) {
    return model.availabilityState !== 'unavailable';
  }
  return discoveredModels.some(entry => entry.id === model.id);
}

function canExecuteBinding({
  purpose,
  account,
  model,
  modelsByAccountId,
}: {
  purpose: AIPurpose;
  account: AIAccount | undefined;
  model: AIModelOption | undefined;
  modelsByAccountId: Record<string, AIModelOption[]>;
}) {
  if (!account || account.state !== 'active' || !model) {
    return false;
  }
  if (model.providerCatalogId !== account.providerId) {
    return false;
  }
  if (!model.allowedBindingPurposes.includes(purpose)) {
    return false;
  }
  if (model.availabilityState === 'unavailable') {
    return false;
  }
  return isModelAvailableForAccount(model, account, modelsByAccountId);
}

export function formatModelLabel(model: AIModelOption, providers: AIProvider[]) {
  const provider = providers.find(entry => entry.id === model.providerCatalogId);
  return provider ? `${provider.displayName} · ${model.modelName}` : model.modelName;
}

export type ModelPriceSummary = {
  inputPerMillion?: number;
  outputPerMillion?: number;
  currency: string;
};

/** Effective per-model price, preferring a workspace override over the base catalog row. */
export function resolveModelPriceSummary(modelId: string, prices: PricingRule[]): ModelPriceSummary | null {
  const forModel = prices.filter(entry => entry.modelCatalogId === modelId);
  if (forModel.length === 0) {
    return null;
  }
  const pick = (unit: string) => {
    const overrides = forModel.filter(entry => entry.billingUnit === unit && entry.sourceOrigin === 'workspace_override');
    const candidates = overrides.length > 0 ? overrides : forModel.filter(entry => entry.billingUnit === unit);
    return candidates[0];
  };
  const input = pick('per_1m_input_tokens');
  const output = pick('per_1m_output_tokens');
  if (!input && !output) {
    return null;
  }
  return {
    ...(input ? { inputPerMillion: input.unitPrice } : {}),
    ...(output ? { outputPerMillion: output.unitPrice } : {}),
    currency: input?.currency ?? output?.currency ?? 'USD',
  };
}

/** Renders as `$0.15/$0.60`; missing sides fall back to an em dash. */
export function formatModelPriceSuffix(summary: ModelPriceSummary | null): string {
  if (!summary) {
    return '';
  }
  const fmt = (value: number | undefined) => (value === undefined ? '—' : `$${value.toFixed(2)}`);
  return `${fmt(summary.inputPerMillion)}/${fmt(summary.outputPerMillion)}`;
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
  availableAccounts,
  localAccounts,
  bindingsForScope,
  instanceBindings,
  workspaceBindings,
  models,
  providers,
  priceRuleCount,
}: {
  selectedScope: AIScopeKind;
  availableAccounts: AIAccount[];
  localAccounts: AIAccount[];
  bindingsForScope: AIBindingAssignment[];
  instanceBindings: AIBindingAssignment[];
  workspaceBindings: AIBindingAssignment[];
  models: AIModelOption[];
  providers: AIProvider[];
  priceRuleCount?: number;
}): AiReadinessSummary {
  const accountById = new Map(availableAccounts.map(entry => [entry.id, entry]));
  const modelById = new Map(models.map(entry => [entry.id, entry]));
  const resolutions = REQUIRED_RUNTIME_PURPOSE_ORDER.map(purpose => ({
    purpose,
    resolution: resolveBindingForPurpose({
      purpose,
      selectedScope,
      bindingsForScope,
      instanceBindings,
      workspaceBindings,
    }),
  }));
  const executablePurposeIds = new Set<AIPurpose>();
  resolutions.forEach(({ purpose, resolution }) => {
    const binding = resolution.effectiveBinding;
    if (!binding || binding.state !== 'configured') {
      return;
    }
    const canExecute = canExecuteBinding({
      purpose,
      account: accountById.get(binding.accountId),
      model: modelById.get(binding.modelCatalogId),
      modelsByAccountId: {},
    });
    if (canExecute) {
      executablePurposeIds.add(purpose);
    }
  });
  const configuredProviderIds = new Set(availableAccounts.map(entry => entry.providerId));

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
    return !canExecuteBinding({
      purpose,
      account: accountById.get(binding.accountId),
      model: modelById.get(binding.modelCatalogId),
      modelsByAccountId: {},
    });
  });

  return {
    totalPurposes: REQUIRED_RUNTIME_PURPOSE_ORDER.length,
    executableEffectiveBindings: executablePurposeIds.size,
    localBindingCount: bindingsForScope.length,
    missingPurposes: REQUIRED_RUNTIME_PURPOSE_ORDER.filter(purpose => !executablePurposeIds.has(purpose)),
    missingOptionalPurposes,
    totalAccountCount: availableAccounts.length,
    activeAccountCount: availableAccounts.filter(entry => entry.state === 'active').length,
    localAccountCount: localAccounts.length,
    visibleModelCount: models.length,
    availableModelCount: models.filter(entry => entry.availabilityState !== 'unavailable').length,
    providerCatalogCount: providers.length,
    configuredProviderCount: configuredProviderIds.size,
    priceRuleCount: priceRuleCount ?? 0,
  };
}

export function recommendAiConfigSection(summary: AiReadinessSummary): AiConfigSection {
  if (summary.activeAccountCount === 0) {
    return 'accounts';
  }
  return 'bindings';
}

export function suggestBindingSelection({
  purpose,
  availableAccounts,
  models,
  preferredAccountId,
  preferredModelCatalogId,
}: {
  purpose: AIPurpose;
  availableAccounts: AIAccount[];
  models: AIModelOption[];
  preferredAccountId?: string | undefined;
  preferredModelCatalogId?: string | undefined;
}): AiBindingSuggestion {
  const purposeModels = models
    .filter(entry => entry.allowedBindingPurposes.includes(purpose) && entry.availabilityState !== 'unavailable');
  const activeAccounts = availableAccounts
    .filter(entry => entry.state === 'active')
    .slice()
    .sort(compareByUpdatedAtDesc);
  const preferredAccount = preferredAccountId
    ? activeAccounts.find(entry => entry.id === preferredAccountId)
    : undefined;
  const preferredModel = preferredModelCatalogId
    ? purposeModels.find(entry => entry.id === preferredModelCatalogId)
    : undefined;
  if (preferredAccount && preferredModel && preferredModel.providerCatalogId === preferredAccount.providerId) {
    return { accountId: preferredAccount.id, modelCatalogId: preferredModel.id };
  }

  for (const account of activeAccounts) {
    const model = purposeModels.find(entry => entry.providerCatalogId === account.providerId);
    if (model) {
      return { accountId: account.id, modelCatalogId: model.id };
    }
  }

  return { accountId: '', modelCatalogId: '' };
}

/**
 * Shared Zod shape for the binding "model + parameters" step, used both by
 * the inline per-purpose editor (BindingPurposeCard) and the guided wizard's
 * second step — both create/update the same `CreateAiBindingRequest` /
 * `UpdateAiBindingRequest` inline-parameter payload.
 */
export function bindingParamsSchema(t: TFunction) {
  return z.object({
    accountId: nonEmptyString(t('admin.aiPanel.fields.account')),
    modelCatalogId: nonEmptyString(t('admin.model')),
    systemPrompt: z.string(),
    temperature: optionalNumberString(t('admin.temperature')),
    topP: optionalNumberString(t('admin.topP')),
    maxOutputTokens: optionalIntegerString(t('admin.maxOutputTokens')),
    extraParametersJson: z.string().superRefine((value, context) => {
      const normalized = value.trim();
      if (!normalized) {
        return;
      }
      try {
        const parsed = JSON.parse(normalized) as unknown;
        if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
          throw new Error('not an object');
        }
      } catch {
        context.addIssue({
          code: 'custom',
          message: t('admin.aiPanel.messages.invalidJson'),
        });
      }
    }),
  });
}

export type BindingParamsFormValues = z.output<ReturnType<typeof bindingParamsSchema>>;

export function bindingParamsRequestBody(values: BindingParamsFormValues) {
  const extraParametersJson = values.extraParametersJson.trim()
    ? (JSON.parse(values.extraParametersJson) as Record<string, unknown>)
    : undefined;
  return {
    accountId: values.accountId,
    modelCatalogId: values.modelCatalogId,
    systemPrompt: values.systemPrompt.trim() || undefined,
    temperature: values.temperature,
    topP: values.topP,
    maxOutputTokensOverride: values.maxOutputTokens,
    ...(extraParametersJson !== undefined ? { extraParametersJson } : {}),
  };
}
