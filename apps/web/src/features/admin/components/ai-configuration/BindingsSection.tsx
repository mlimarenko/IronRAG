import { useEffect, useMemo, useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { toast } from 'sonner'

import { adminApi, adminModelCatalogOptions } from '@/shared/api'
import type { AiBindingPurpose, AiBindingResponse } from '@/shared/api/generated'
import { DataState } from '@/shared/components/DataState'
import { Badge } from '@/shared/components/ui/badge'
import { errorMessage } from '@/shared/lib/errorMessage'
import { shouldRefreshCredentialModels } from '@/shared/lib/ai-provider'
import type { AIAccount, AIBinding, AIModelOption, AIScopeKind, PricingRule } from '@/shared/types'
import { mapModelList } from '@/features/admin/model/aiAdapter'
import {
  OPTIONAL_BINDING_PURPOSES,
  REQUIRED_BINDING_PURPOSES,
  bindingParamsRequestBody,
  bindingParamsSchema,
  type BindingParamsFormValues,
  compactScopeQuery,
  isBindingExecutable,
  localScopeQuery,
  modelCatalogScopeQuery,
  resolveBindingForPurpose,
  suggestBindingSelection,
  visibleScopeQuery,
  type AccountModelLoadState,
  type AiConfigDataState,
  type AiScopeContext,
} from '@/features/admin/model/aiConfig'
import { useTypedForm } from '@/shared/forms'
import { BindingPurposeCard } from './BindingPurposeCard'
import { adminAiBindingsQueryKey } from './useAiConfigQueries'

type BindingsSectionProps = {
  selectedScope: AIScopeKind
  scopeContext: AiScopeContext
  bindingsState: AiConfigDataState<{ ready: true }>
  availableAccounts: AIAccount[]
  localAccounts: AIAccount[]
  models: AIModelOption[]
  prices: PricingRule[]
  bindingsForScope: AIBinding[]
  instanceBindings: AIBinding[]
  workspaceBindings: AIBinding[]
  modelById: Map<string, AIModelOption>
  invalidateAll: () => Promise<void>
}

type BindingMutationContext = {
  previousBindings: AiBindingResponse[] | undefined
}

type BindingScopeQuery = ReturnType<typeof compactScopeQuery>

type BindingSaveVariables = {
  bindingId: string | null
  optimisticId: string
  purpose: AiBindingPurpose
  scopeKind: AIScopeKind
  scopeQuery: BindingScopeQuery
  values: BindingParamsFormValues
}

type BindingResetVariables = {
  bindingId: string
  purpose: AiBindingPurpose
  scopeQuery: BindingScopeQuery
}

function buildOptimisticBinding({
  bindingId,
  optimisticId,
  purpose,
  scopeKind,
  scopeQuery,
  values,
}: BindingSaveVariables): AiBindingResponse {
  const body = bindingParamsRequestBody(values)
  return {
    id: bindingId ?? optimisticId,
    scopeKind,
    bindingPurpose: purpose,
    bindingState: 'active',
    accountId: body.accountId,
    modelCatalogId: body.modelCatalogId,
    systemPrompt: body.systemPrompt ?? null,
    temperature: body.temperature ?? null,
    topP: body.topP ?? null,
    maxOutputTokensOverride: body.maxOutputTokensOverride ?? null,
    extraParametersJson: body.extraParametersJson ?? null,
    ...(scopeQuery.workspaceId ? { workspaceId: scopeQuery.workspaceId } : {}),
    ...(scopeQuery.libraryId ? { libraryId: scopeQuery.libraryId } : {}),
  }
}

function applyOptimisticBinding(
  current: AiBindingResponse[] | undefined,
  binding: AiBindingResponse,
): AiBindingResponse[] {
  return [
    ...(current ?? []).filter(
      (entry) => entry.id !== binding.id && entry.bindingPurpose !== binding.bindingPurpose,
    ),
    binding,
  ]
}

function accountModelLoadState(
  isLoading: boolean,
  hasError: boolean,
  hasModels: boolean,
): AccountModelLoadState | undefined {
  if (isLoading) return 'loading'
  if (hasError) return 'failed'
  return hasModels ? 'ready' : undefined
}

function bindingFormValuesFromExisting(existing: AIBinding) {
  return {
    accountId: existing.accountId,
    modelCatalogId: existing.modelCatalogId,
    systemPrompt: existing.systemPrompt ?? '',
    temperature: existing.temperature != null ? String(existing.temperature) : '',
    topP: existing.topP != null ? String(existing.topP) : '',
    maxOutputTokens: existing.maxOutputTokens != null ? String(existing.maxOutputTokens) : '',
    extraParametersJson: existing.extraParams ? JSON.stringify(existing.extraParams, null, 2) : '',
  }
}

export function BindingsSection({
  selectedScope,
  scopeContext,
  bindingsState,
  availableAccounts,
  localAccounts,
  models,
  prices,
  bindingsForScope,
  instanceBindings,
  workspaceBindings,
  modelById,
  invalidateAll,
}: Readonly<BindingsSectionProps>) {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const [editingPurpose, setEditingPurpose] = useState<AiBindingPurpose | null>(null)
  const bindingSchema = useMemo(() => bindingParamsSchema(t), [t])
  const bindingForm = useTypedForm({
    schema: bindingSchema,
    defaultValues: {
      accountId: '',
      modelCatalogId: '',
      systemPrompt: '',
      temperature: '',
      topP: '',
      maxOutputTokens: '',
      extraParametersJson: '',
    },
    mode: 'onChange',
  })
  const { reset: resetBindingForm, setValue: setBindingValue, watch: watchBinding } = bindingForm
  const bindingAccountId = watchBinding('accountId')
  const localScopeParams = useMemo(
    () => compactScopeQuery(localScopeQuery(selectedScope, scopeContext).query),
    [scopeContext, selectedScope],
  )

  const selectedAccount = useMemo(
    () =>
      bindingAccountId
        ? (availableAccounts.find((entry) => entry.id === bindingAccountId) ?? null)
        : null,
    [availableAccounts, bindingAccountId],
  )
  const selectedAccountModelsQueryParams = {
    ...(selectedAccount
      ? {
          providerCatalogId: selectedAccount.providerId,
          accountId: selectedAccount.id,
        }
      : {}),
    ...modelCatalogScopeQuery(visibleScopeQuery(selectedScope, scopeContext).query),
  }
  const selectedAccountModelsQuery = useQuery({
    ...adminModelCatalogOptions(selectedAccountModelsQueryParams),
    enabled: Boolean(editingPurpose) && shouldRefreshCredentialModels(selectedAccount?.provider),
  })
  const selectedAccountModels = useMemo<AIModelOption[] | null>(
    () => (selectedAccountModelsQuery.data ? mapModelList(selectedAccountModelsQuery.data) : null),
    [selectedAccountModelsQuery.data],
  )
  const modelsByAccountId = useMemo<Record<string, AIModelOption[]>>(() => {
    if (!selectedAccount || !selectedAccountModels) {
      return {}
    }
    return { [selectedAccount.id]: selectedAccountModels }
  }, [selectedAccount, selectedAccountModels])
  const selectedAccountLoadState = accountModelLoadState(
    selectedAccountModelsQuery.isLoading || selectedAccountModelsQuery.isFetching,
    Boolean(selectedAccountModelsQuery.error),
    selectedAccountModels !== null,
  )

  useEffect(() => {
    if (selectedAccountModelsQuery.error) {
      toast.error(t('admin.aiPanel.messages.accountModelRefreshFailed'))
    }
  }, [selectedAccountModelsQuery.error, t])

  const resolveBinding = (purpose: AiBindingPurpose) =>
    resolveBindingForPurpose({
      purpose,
      selectedScope,
      bindingsForScope,
      instanceBindings,
      workspaceBindings,
    })
  const openBindingEditor = (purpose: AiBindingPurpose) => {
    const resolved = resolveBinding(purpose)
    if (resolved.localBinding) {
      resetBindingForm(bindingFormValuesFromExisting(resolved.localBinding))
    } else {
      const suggestion = suggestBindingSelection({
        purpose,
        availableAccounts,
        models,
        preferredAccountId: resolved.effectiveBinding?.accountId,
        preferredModelCatalogId: resolved.effectiveBinding?.modelCatalogId,
      })
      resetBindingForm({
        accountId: suggestion.accountId,
        modelCatalogId: suggestion.modelCatalogId,
        systemPrompt: '',
        temperature: '',
        topP: '',
        maxOutputTokens: '',
        extraParametersJson: '',
      })
    }
    setEditingPurpose(purpose)
  }

  const saveBindingMutation = useMutation<
    AiBindingResponse,
    unknown,
    BindingSaveVariables,
    BindingMutationContext
  >({
    mutationKey: ['admin', 'ai', 'bindings', 'save'],
    scope: {
      id: `admin:ai:bindings:${selectedScope}:${scopeContext.workspaceId ?? 'instance'}:${scopeContext.libraryId ?? 'none'}`,
    },
    mutationFn: (variables) =>
      variables.bindingId
        ? adminApi.updateBinding(variables.bindingId, {
            ...bindingParamsRequestBody(variables.values),
            bindingState: 'active',
          })
        : adminApi.createBinding({
            ...variables.scopeQuery,
            scopeKind: variables.scopeKind,
            bindingPurpose: variables.purpose,
            ...bindingParamsRequestBody(variables.values),
          }),
    onMutate: async (variables) => {
      const queryKey = adminAiBindingsQueryKey(variables.scopeQuery)
      await queryClient.cancelQueries({ queryKey })
      const previousBindings = queryClient.getQueryData<AiBindingResponse[]>(queryKey)
      queryClient.setQueryData<AiBindingResponse[]>(queryKey, (current) =>
        applyOptimisticBinding(current, buildOptimisticBinding(variables)),
      )
      setEditingPurpose(null)
      return { previousBindings }
    },
    onSuccess: (binding, variables) => {
      queryClient.setQueryData<AiBindingResponse[]>(
        adminAiBindingsQueryKey(variables.scopeQuery),
        (current = []) =>
          current.map((entry) =>
            entry.id === variables.optimisticId || entry.id === binding.id ? binding : entry,
          ),
      )
      toast.success(t('admin.aiPanel.messages.bindingSaved'))
    },
    onError: (err, variables, context) => {
      if (context) {
        queryClient.setQueryData(
          adminAiBindingsQueryKey(variables.scopeQuery),
          context.previousBindings,
        )
      }
      toast.error(
        t('admin.aiPanel.messages.bindingRollbackFailed', {
          error: errorMessage(err, t('admin.aiPanel.messages.bindingSaveFailed')),
        }),
      )
    },
    onSettled: async () => {
      await invalidateAll()
    },
  })

  const resetBindingMutation = useMutation<
    void,
    unknown,
    BindingResetVariables,
    BindingMutationContext
  >({
    mutationKey: ['admin', 'ai', 'bindings', 'reset'],
    scope: {
      id: `admin:ai:bindings:${selectedScope}:${scopeContext.workspaceId ?? 'instance'}:${scopeContext.libraryId ?? 'none'}`,
    },
    mutationFn: ({ bindingId }) => adminApi.deleteBinding(bindingId),
    onMutate: async (variables) => {
      const queryKey = adminAiBindingsQueryKey(variables.scopeQuery)
      await queryClient.cancelQueries({ queryKey })
      const previousBindings = queryClient.getQueryData<AiBindingResponse[]>(queryKey)
      queryClient.setQueryData<AiBindingResponse[]>(queryKey, (current = []) =>
        current.filter(
          (entry) => entry.id !== variables.bindingId && entry.bindingPurpose !== variables.purpose,
        ),
      )
      setEditingPurpose(null)
      return { previousBindings }
    },
    onSuccess: () => {
      toast.success(t('admin.aiPanel.messages.overrideRemoved'))
    },
    onError: (err, variables, context) => {
      if (context) {
        queryClient.setQueryData(
          adminAiBindingsQueryKey(variables.scopeQuery),
          context.previousBindings,
        )
      }
      toast.error(
        t('admin.aiPanel.messages.bindingRollbackFailed', {
          error: errorMessage(err, t('admin.aiPanel.messages.overrideRemoveFailed')),
        }),
      )
    },
    onSettled: async () => {
      await invalidateAll()
    },
  })

  const saveBinding = (purpose: AiBindingPurpose) => {
    const resolved = resolveBinding(purpose)
    return bindingForm.handleSubmit((values) => {
      saveBindingMutation.mutate({
        bindingId: resolved.localBinding?.id ?? null,
        optimisticId: `optimistic-binding-${selectedScope}-${purpose}`,
        purpose,
        scopeKind: selectedScope,
        scopeQuery: localScopeParams,
        values,
      })
    })()
  }
  const resetBinding = async (purpose: AiBindingPurpose) => {
    const resolved = resolveBinding(purpose)
    if (!resolved.localBinding || selectedScope === 'instance') {
      return
    }
    resetBindingMutation.mutate({
      bindingId: resolved.localBinding.id,
      purpose,
      scopeQuery: localScopeParams,
    })
  }

  const showMissingInstanceNotice =
    selectedScope !== 'instance' &&
    instanceBindings.length === 0 &&
    localAccounts.length + bindingsForScope.length > 0
  const purposeIsExecutable = (purpose: AiBindingPurpose) => {
    const binding = resolveBinding(purpose).effectiveBinding
    if (binding?.state !== 'active') {
      return false
    }
    return isBindingExecutable({
      purpose,
      account: availableAccounts.find((entry) => entry.id === binding.accountId),
      model: modelById.get(binding.modelCatalogId),
      modelsByAccountId: {},
    })
  }
  const configuredRequiredBindings = REQUIRED_BINDING_PURPOSES.filter(purposeIsExecutable).length
  const configuredOptionalBindings = OPTIONAL_BINDING_PURPOSES.filter(purposeIsExecutable).length
  const renderPurpose = (purpose: AiBindingPurpose) => {
    const resolved = resolveBinding(purpose)
    return (
      <BindingPurposeCard
        key={purpose}
        purpose={purpose}
        selectedScope={selectedScope}
        resolved={resolved}
        availableAccounts={availableAccounts}
        models={models}
        prices={prices}
        modelById={modelById}
        modelsByAccountId={modelsByAccountId}
        selectedAccount={selectedAccount}
        selectedAccountLoadState={selectedAccountLoadState}
        editing={editingPurpose === purpose}
        form={bindingForm}
        bindingSaving={saveBindingMutation.isPending || resetBindingMutation.isPending}
        onAccountChange={(value) => {
          setBindingValue('accountId', value, { shouldDirty: true, shouldValidate: true })
          setBindingValue('modelCatalogId', '', { shouldDirty: true, shouldValidate: true })
        }}
        onOpen={() => openBindingEditor(purpose)}
        onCancel={() => setEditingPurpose(null)}
        onSave={() => saveBinding(purpose)}
        onReset={async () => {
          await resetBinding(purpose)
        }}
      />
    )
  }
  const renderPurposeGroup = ({
    title,
    description,
    purposes,
    configuredCount,
  }: {
    title: string
    description?: string
    purposes: readonly AiBindingPurpose[]
    configuredCount: number
  }) => (
    <section className="space-y-2">
      <div className="min-w-0">
        <div className="flex flex-wrap items-center gap-2">
          <h3 className="text-sm font-bold tracking-tight">{title}</h3>
          <Badge variant="outline">
            {configuredCount}/{purposes.length}
          </Badge>
        </div>
        {description && (
          <p className="mt-1 max-w-4xl text-sm leading-5 text-muted-foreground">{description}</p>
        )}
      </div>
      <div className="workbench-surface overflow-hidden">{purposes.map(renderPurpose)}</div>
    </section>
  )

  return (
    <DataState query={bindingsState}>
      {() => (
        <div className="space-y-3">
          {showMissingInstanceNotice && (
            <div className="rounded-md border border-status-warning/20 bg-status-warning/5 p-3 text-sm text-status-warning">
              {t('admin.aiPanel.notices.missingInstanceBaseline')}
            </div>
          )}
          {renderPurposeGroup({
            title: t('admin.aiPanel.sections.requiredBindingsTitle'),
            purposes: REQUIRED_BINDING_PURPOSES,
            configuredCount: configuredRequiredBindings,
          })}
          {renderPurposeGroup({
            title: t('admin.aiPanel.sections.optionalBindingsTitle'),
            description: t('admin.aiPanel.sections.optionalBindingsDescription'),
            purposes: OPTIONAL_BINDING_PURPOSES,
            configuredCount: configuredOptionalBindings,
          })}
        </div>
      )}
    </DataState>
  )
}
