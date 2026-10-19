import type { TFunction } from 'i18next'

import type { AiBindingPurpose } from '@/shared/api/generated'
import type { AIAccount, AIBinding, AIModelOption, AIProvider, AIScopeKind } from '@/shared/types'

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

export type AiReadinessSummary = {
  totalPurposes: number
  executableEffectiveBindings: number
  localBindingCount: number
  missingPurposes: AiBindingPurpose[]
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

export const BINDING_PURPOSES = [
  'extract_text',
  'extract_graph',
  'graph_entity_type',
  'embed_chunk',
  'query_compile',
  'query_rerank',
  'query_answer',
  'agent',
] as const satisfies readonly AiBindingPurpose[]

const KNOWLEDGE_WORKFLOW_BINDING_PURPOSES = [
  'extract_graph',
  'embed_chunk',
  'query_compile',
  'query_answer',
] as const satisfies readonly AiBindingPurpose[]

export function purposeLabel(value: AiBindingPurpose, t: TFunction) {
  return t(`admin.aiPanel.purposeLabels.${value}`)
}

export function purposeDescription(value: AiBindingPurpose, t: TFunction) {
  return t(`admin.aiPanel.purposeDescriptions.${value}`)
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
  if (!providerServesPurpose(account.provider, purpose)) {
    return false
  }
  if (!modelSupportsBindingPurpose(model, purpose)) {
    return false
  }
  if (model.availabilityState === 'unavailable') {
    return false
  }
  return isModelAvailableForAccount(model, account, modelsByAccountId)
}

export function modelSupportsBindingPurpose(model: AIModelOption, purpose: AiBindingPurpose) {
  return model.allowedBindingPurposes.includes(purpose)
}

/**
 * The decision purposes a decision-only provider may serve (IRG-182). Mirrors
 * `AiBindingPurpose::is_decision` on the backend: `query_rerank` and
 * `graph_entity_type`. Every other purpose is generative.
 */
export function isDecisionBindingPurpose(purpose: AiBindingPurpose): boolean {
  return purpose === 'query_rerank' || purpose === 'graph_entity_type'
}

/**
 * Whether a provider may be offered for a purpose. A provider that declares the
 * typed `decision` capability is decision-only: the server rejects it for every
 * generative purpose, so the account picker must not offer it there. A provider
 * that does not declare the capability is unaffected, and a chat model stays
 * eligible for a decision purpose through its own allowed purposes.
 */
export function providerServesPurpose(
  provider: AIProvider | undefined,
  purpose: AiBindingPurpose | null,
): boolean {
  if (provider?.capabilities?.decision !== 'supported') {
    return true
  }
  return purpose !== null && isDecisionBindingPurpose(purpose)
}

function compareByUpdatedAtDesc(
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
  const resolutions = KNOWLEDGE_WORKFLOW_BINDING_PURPOSES.map((purpose) => ({
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

  return {
    totalPurposes: KNOWLEDGE_WORKFLOW_BINDING_PURPOSES.length,
    executableEffectiveBindings: executablePurposeIds.size,
    localBindingCount: bindingsForScope.length,
    missingPurposes: KNOWLEDGE_WORKFLOW_BINDING_PURPOSES.filter(
      (purpose) => !executablePurposeIds.has(purpose),
    ),
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
    .filter((entry) => entry.state === 'active' && providerServesPurpose(entry.provider, purpose))
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
 * The scope the AI configuration screen can actually show, given what the URL
 * asked for and what is currently selected. A deep link to `?scope=library`
 * arriving without an active library must not render an empty screen: it falls
 * back to the nearest scope that exists, workspace before instance.
 */
export function reachableAiScope(
  requested: AIScopeKind,
  hasWorkspace: boolean,
  hasLibrary: boolean,
): AIScopeKind {
  if (requested === 'library' && !hasLibrary) return hasWorkspace ? 'workspace' : 'instance'
  if (requested === 'workspace' && !hasWorkspace) return 'instance'
  return requested
}

/**
 * Which of the three things the AI readiness banner has to say. Accounts come
 * first: without a credential no binding can work, so "no bindings" would be
 * the wrong thing to put in front of an operator who has not connected a
 * provider yet.
 */
export type AiReadinessStage = 'missingAccounts' | 'ready' | 'missingBindings'

export function aiReadinessStage(activeAccountCount: number, ready: boolean): AiReadinessStage {
  if (activeAccountCount === 0) return 'missingAccounts'
  return ready ? 'ready' : 'missingBindings'
}
