import type { TFunction } from 'i18next'
import { z } from 'zod'

import { nonEmptyString, optionalIntegerString, optionalNumberString } from '@/shared/forms'
import type { AiBindingPurpose } from '@/shared/api/generated'
import type {
  AIAccount,
  AIBinding,
  AIModelOption,
  AIProvider,
  AIScopeKind,
  PricingRule,
} from '@/shared/types'

export type AiConfigSection = 'bindings' | 'accounts' | 'catalog'

export function isProviderCredentialValidationFailure(error: unknown): boolean {
  if (!error || typeof error !== 'object') {
    return false
  }
  const body = (error as { body?: unknown }).body
  if (!body || typeof body !== 'object') {
    return false
  }
  const errorKind = (body as { errorKind?: unknown }).errorKind
  return (
    typeof errorKind === 'string' && /^provider_credential_validation_[a-z0-9_]+$/.test(errorKind)
  )
}

export type AiCatalogTab = 'providers' | 'models'

export type AiConfigDataState<T> = {
  isLoading: boolean
  error: unknown
  data: T | undefined
}

export type AiScopeContext = {
  workspaceId?: string | undefined
  libraryId?: string | undefined
}

export type AiScopeQueryParams = {
  query?: {
    scopeKind?: AIScopeKind
    workspaceId?: string | undefined
    libraryId?: string | undefined
  }
}

export type LocalAiScopeQueryParams = {
  query: {
    scopeKind: AIScopeKind
    workspaceId?: string | undefined
    libraryId?: string | undefined
  }
}

export type DefinedAiScopeQuery = {
  scopeKind?: AIScopeKind
  workspaceId?: string
  libraryId?: string
}

export type BindingResolution = {
  localBinding: AIBinding | null
  effectiveBinding: AIBinding | null
  sourceKind: AIScopeKind | null
}

export type AccountModelLoadState = 'loading' | 'ready' | 'failed'

export type AiReadinessSummary = {
  totalPurposes: number
  executableEffectiveBindings: number
  localBindingCount: number
  missingPurposes: AiBindingPurpose[]
  /** Optional bindings that are not configured — the system falls back
   *  to local CPU processing (lower quality / higher latency). */
  missingOptionalPurposes: AiBindingPurpose[]
  totalAccountCount: number
  activeAccountCount: number
  localAccountCount: number
  visibleModelCount: number
  availableModelCount: number
  providerCatalogCount: number
  configuredProviderCount: number
  priceRuleCount: number
}

export type AiBindingSuggestion = {
  accountId: string
  modelCatalogId: string
}

export const REQUIRED_BINDING_PURPOSES = [
  'extract_graph',
  'embed_chunk',
  'query_compile',
  'query_answer',
  'agent',
] as const satisfies readonly AiBindingPurpose[]

export const OPTIONAL_BINDING_PURPOSES = [
  'extract_text',
] as const satisfies readonly AiBindingPurpose[]

export function purposeLabel(value: AiBindingPurpose, t: TFunction) {
  return t(`admin.aiPanel.purposeLabels.${value}`)
}

export function purposeDescription(value: AiBindingPurpose, t: TFunction) {
  return t(`admin.aiPanel.purposeDescriptions.${value}`)
}

export function scopeLabel(value: AIScopeKind, t: TFunction) {
  return t(`admin.aiPanel.scopeLabels.${value}`)
}

export function accountStateLabel(value: AIAccount['state'], t: TFunction) {
  return t(`admin.aiPanel.accountStateLabels.${value}`)
}

export function localScopeQuery(
  scopeKind: AIScopeKind,
  context: AiScopeContext,
): LocalAiScopeQueryParams {
  if (scopeKind === 'instance') {
    return { query: { scopeKind } }
  }
  if (scopeKind === 'workspace') {
    return { query: { scopeKind, workspaceId: context.workspaceId } }
  }
  return { query: { scopeKind, workspaceId: context.workspaceId, libraryId: context.libraryId } }
}

export function visibleScopeQuery(
  scopeKind: AIScopeKind,
  context: AiScopeContext,
): AiScopeQueryParams {
  if (scopeKind === 'instance') {
    return {}
  }
  if (scopeKind === 'workspace') {
    return { query: { workspaceId: context.workspaceId } }
  }
  return { query: { workspaceId: context.workspaceId, libraryId: context.libraryId } }
}

export function compactScopeQuery(params: AiScopeQueryParams['query']): DefinedAiScopeQuery {
  return {
    ...(params?.scopeKind ? { scopeKind: params.scopeKind } : {}),
    ...(params?.workspaceId ? { workspaceId: params.workspaceId } : {}),
    ...(params?.libraryId ? { libraryId: params.libraryId } : {}),
  }
}

export function modelCatalogScopeQuery(params: AiScopeQueryParams['query']) {
  return {
    ...(params?.workspaceId ? { workspaceId: params.workspaceId } : {}),
    ...(params?.libraryId ? { libraryId: params.libraryId } : {}),
  }
}

export function isModelAvailableForAccount(
  model: AIModelOption | undefined,
  account: AIAccount | null | undefined,
  modelsByAccountId: Record<string, AIModelOption[]>,
): boolean {
  if (!model || !account) {
    return true
  }
  const discoveredModels = modelsByAccountId[account.id]
  if (!discoveredModels) {
    return model.availabilityState !== 'unavailable'
  }
  return discoveredModels.some((entry) => entry.id === model.id)
}

export function isBindingExecutable({
  purpose,
  account,
  model,
  modelsByAccountId,
}: {
  purpose: AiBindingPurpose
  account: AIAccount | undefined
  model: AIModelOption | undefined
  modelsByAccountId: Record<string, AIModelOption[]>
}) {
  if (account?.state !== 'active' || !model) {
    return false
  }
  if (model.providerCatalogId !== account.providerId) {
    return false
  }
  if (!modelSupportsBindingPurpose(model, purpose)) {
    return false
  }
  if (!providerSupportsBindingPurpose(account.provider, purpose)) {
    return false
  }
  if (model.availabilityState === 'unavailable') {
    return false
  }
  return isModelAvailableForAccount(model, account, modelsByAccountId)
}

export function modelSupportsBindingPurpose(model: AIModelOption, purpose: AiBindingPurpose) {
  const requiredCapabilityKind = purpose === 'embed_chunk' ? 'embedding' : 'chat'
  return (
    model.allowedBindingPurposes.includes(purpose) &&
    model.capabilityKind === requiredCapabilityKind &&
    (purpose !== 'extract_text' || model.modalityKind === 'multimodal')
  )
}

export function providerSupportsBindingPurpose(
  provider: AIProvider | null | undefined,
  purpose: AiBindingPurpose,
) {
  if (!provider) {
    return false
  }
  switch (purpose) {
    case 'extract_text':
      return (
        provider.capabilities.chat === 'supported' && provider.capabilities.vision === 'supported'
      )
    case 'embed_chunk':
      return provider.capabilities.embeddings === 'supported'
    case 'agent':
      return (
        provider.capabilities.chat === 'supported' && provider.capabilities.tools === 'supported'
      )
    case 'extract_graph':
    case 'query_compile':
    case 'query_answer':
      return provider.capabilities.chat === 'supported'
  }
}

export function formatModelLabel(model: AIModelOption, providers: AIProvider[]) {
  const provider = providers.find((entry) => entry.id === model.providerCatalogId)
  return provider ? `${provider.displayName} · ${model.modelName}` : model.modelName
}

export type ModelPriceSummary = {
  inputPerMillion?: number
  outputPerMillion?: number
  currency: string
}

/** Effective per-model price, preferring a workspace override over the base catalog row. */
export function resolveModelPriceSummary(
  modelId: string,
  prices: PricingRule[],
): ModelPriceSummary | null {
  const forModel = prices.filter((entry) => entry.modelCatalogId === modelId)
  if (forModel.length === 0) {
    return null
  }
  const pick = (unit: string) => {
    const overrides = forModel.filter(
      (entry) => entry.billingUnit === unit && entry.sourceOrigin === 'workspace_override',
    )
    const candidates =
      overrides.length > 0 ? overrides : forModel.filter((entry) => entry.billingUnit === unit)
    return candidates[0]
  }
  const input = pick('per_1m_input_tokens')
  const output = pick('per_1m_output_tokens')
  if (!input && !output) {
    return null
  }
  return {
    ...(input ? { inputPerMillion: input.unitPrice } : {}),
    ...(output ? { outputPerMillion: output.unitPrice } : {}),
    currency: input?.currency ?? output?.currency ?? 'USD',
  }
}

/** Renders as `$0.15/$0.60`; missing sides fall back to an em dash. */
export function formatModelPriceSuffix(summary: ModelPriceSummary | null): string {
  if (!summary) {
    return ''
  }
  const fmt = (value: number | undefined) => (value === undefined ? '—' : `$${value.toFixed(2)}`)
  return `${fmt(summary.inputPerMillion)}/${fmt(summary.outputPerMillion)}`
}

export function matchesFilter(values: Array<string | undefined>, filter: string) {
  const normalized = filter.trim().toLocaleLowerCase()
  if (!normalized) {
    return true
  }
  return values.some((value) => value?.toLocaleLowerCase().includes(normalized))
}

export function compareByUpdatedAtDesc(
  left: { updatedAt: string; id: string },
  right: { updatedAt: string; id: string },
) {
  return right.updatedAt.localeCompare(left.updatedAt) || left.id.localeCompare(right.id)
}

export function resolveBindingForPurpose({
  purpose,
  selectedScope,
  bindingsForScope,
  instanceBindings,
  workspaceBindings,
}: {
  purpose: AiBindingPurpose
  selectedScope: AIScopeKind
  bindingsForScope: AIBinding[]
  instanceBindings: AIBinding[]
  workspaceBindings: AIBinding[]
}): BindingResolution {
  const localBinding = bindingsForScope.find((entry) => entry.purpose === purpose) ?? null
  if (localBinding) {
    return { localBinding, effectiveBinding: localBinding, sourceKind: selectedScope }
  }
  if (selectedScope === 'library') {
    const workspaceBinding = workspaceBindings.find((entry) => entry.purpose === purpose) ?? null
    if (workspaceBinding) {
      return { localBinding: null, effectiveBinding: workspaceBinding, sourceKind: 'workspace' }
    }
  }
  const instanceBinding = instanceBindings.find((entry) => entry.purpose === purpose) ?? null
  return {
    localBinding: null,
    effectiveBinding: instanceBinding,
    sourceKind: instanceBinding ? 'instance' : null,
  }
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
  selectedScope: AIScopeKind
  availableAccounts: AIAccount[]
  localAccounts: AIAccount[]
  bindingsForScope: AIBinding[]
  instanceBindings: AIBinding[]
  workspaceBindings: AIBinding[]
  models: AIModelOption[]
  providers: AIProvider[]
  priceRuleCount?: number
}): AiReadinessSummary {
  const accountById = new Map(availableAccounts.map((entry) => [entry.id, entry]))
  const modelById = new Map(models.map((entry) => [entry.id, entry]))
  const resolutions = REQUIRED_BINDING_PURPOSES.map((purpose) => ({
    purpose,
    resolution: resolveBindingForPurpose({
      purpose,
      selectedScope,
      bindingsForScope,
      instanceBindings,
      workspaceBindings,
    }),
  }))
  const executablePurposeIds = new Set<AiBindingPurpose>()
  resolutions.forEach(({ purpose, resolution }) => {
    const binding = resolution.effectiveBinding
    if (binding?.state !== 'active') {
      return
    }
    const canExecute = isBindingExecutable({
      purpose,
      account: accountById.get(binding.accountId),
      model: modelById.get(binding.modelCatalogId),
      modelsByAccountId: {},
    })
    if (canExecute) {
      executablePurposeIds.add(purpose)
    }
  })
  const configuredProviderIds = new Set(availableAccounts.map((entry) => entry.providerId))

  // Optional bindings — check separately from required runtime purposes.
  const missingOptionalPurposes = OPTIONAL_BINDING_PURPOSES.filter((purpose) => {
    const resolution = resolveBindingForPurpose({
      purpose,
      selectedScope,
      bindingsForScope,
      instanceBindings,
      workspaceBindings,
    })
    const binding = resolution.effectiveBinding
    if (binding?.state !== 'active') return true
    return !isBindingExecutable({
      purpose,
      account: accountById.get(binding.accountId),
      model: modelById.get(binding.modelCatalogId),
      modelsByAccountId: {},
    })
  })

  return {
    totalPurposes: REQUIRED_BINDING_PURPOSES.length,
    executableEffectiveBindings: executablePurposeIds.size,
    localBindingCount: bindingsForScope.length,
    missingPurposes: REQUIRED_BINDING_PURPOSES.filter(
      (purpose) => !executablePurposeIds.has(purpose),
    ),
    missingOptionalPurposes,
    totalAccountCount: availableAccounts.length,
    activeAccountCount: availableAccounts.filter((entry) => entry.state === 'active').length,
    localAccountCount: localAccounts.length,
    visibleModelCount: models.length,
    availableModelCount: models.filter((entry) => entry.availabilityState !== 'unavailable').length,
    providerCatalogCount: providers.length,
    configuredProviderCount: configuredProviderIds.size,
    priceRuleCount: priceRuleCount ?? 0,
  }
}

export function recommendAiConfigSection(summary: AiReadinessSummary): AiConfigSection {
  if (summary.activeAccountCount === 0) {
    return 'accounts'
  }
  return 'bindings'
}

export function suggestBindingSelection({
  purpose,
  availableAccounts,
  models,
  preferredAccountId,
  preferredModelCatalogId,
}: {
  purpose: AiBindingPurpose
  availableAccounts: AIAccount[]
  models: AIModelOption[]
  preferredAccountId?: string | undefined
  preferredModelCatalogId?: string | undefined
}): AiBindingSuggestion {
  const purposeModels = models.filter(
    (entry) =>
      modelSupportsBindingPurpose(entry, purpose) && entry.availabilityState !== 'unavailable',
  )
  const activeAccounts = availableAccounts
    .filter((entry) => entry.state === 'active')
    .slice()
    .sort(compareByUpdatedAtDesc)
  const preferredAccount = preferredAccountId
    ? activeAccounts.find((entry) => entry.id === preferredAccountId)
    : undefined
  const preferredModel = preferredModelCatalogId
    ? purposeModels.find((entry) => entry.id === preferredModelCatalogId)
    : undefined
  if (
    preferredAccount &&
    preferredModel &&
    isBindingExecutable({
      purpose,
      account: preferredAccount,
      model: preferredModel,
      modelsByAccountId: {},
    })
  ) {
    return { accountId: preferredAccount.id, modelCatalogId: preferredModel.id }
  }

  for (const account of activeAccounts) {
    const model = purposeModels.find((entry) =>
      isBindingExecutable({
        purpose,
        account,
        model: entry,
        modelsByAccountId: {},
      }),
    )
    if (model) {
      return { accountId: account.id, modelCatalogId: model.id }
    }
  }

  return { accountId: '', modelCatalogId: '' }
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
      const normalized = value.trim()
      if (!normalized) {
        return
      }
      try {
        const parsed = JSON.parse(normalized) as unknown
        if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
          throw new Error('not an object')
        }
      } catch {
        context.addIssue({
          code: 'custom',
          message: t('admin.aiPanel.messages.invalidJson'),
        })
      }
    }),
  })
}

export type BindingParamsFormValues = z.output<ReturnType<typeof bindingParamsSchema>>

export function bindingParamsRequestBody(values: BindingParamsFormValues) {
  const extraParametersJson = values.extraParametersJson.trim()
    ? (JSON.parse(values.extraParametersJson) as Record<string, unknown>)
    : undefined
  return {
    accountId: values.accountId,
    modelCatalogId: values.modelCatalogId,
    systemPrompt: values.systemPrompt.trim() || undefined,
    temperature: values.temperature,
    topP: values.topP,
    maxOutputTokensOverride: values.maxOutputTokens,
    ...(extraParametersJson !== undefined ? { extraParametersJson } : {}),
  }
}
