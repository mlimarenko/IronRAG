import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'

import { adminApi, queries } from '@/shared/api'
import type { BindingValidationResponse } from '@/shared/api/generated'
import type {
  CreateAccountRequest,
  CreateBindingRequest,
  CreateModelRequest,
  CreateProviderRequest,
  UpdateAccountRequest,
  UpdateBindingRequest,
  UpdateModelRequest,
  UpdateProviderRequest,
} from '@/shared/types/api-requests'
import type { AIScopeKind } from '@/shared/types'
import { mapAccountList, mapBindingList, mapModelList, mapProviderList } from '../model/aiAdapter'
import {
  compactScopeQuery,
  localScopeQuery,
  modelCatalogScopeQuery,
  visibleScopeQuery,
  type AiScopeContext,
} from '../model/aiConfig'

type QueryLike = { queryKey: readonly unknown[] }

export function isOperation(operationId: string) {
  return (query: QueryLike) => {
    const head = query.queryKey[0]
    return Boolean(head && typeof head === 'object' && '_id' in head && head._id === operationId)
  }
}

function useInvalidateOperations() {
  const queryClient = useQueryClient()
  return (...operationIds: string[]) =>
    queryClient.invalidateQueries({
      predicate: (query) => operationIds.some((operationId) => isOperation(operationId)(query)),
    })
}

export function useAiAdminData(selectedScope: AIScopeKind, scopeContext: AiScopeContext) {
  const localParams = compactScopeQuery(localScopeQuery(selectedScope, scopeContext).query)
  const visibleParams = compactScopeQuery(visibleScopeQuery(selectedScope, scopeContext).query)
  const modelParams = modelCatalogScopeQuery(visibleScopeQuery(selectedScope, scopeContext).query)
  const instanceParams = { scopeKind: 'instance' as const }
  const workspaceParams = compactScopeQuery({
    scopeKind: 'workspace',
    workspaceId: scopeContext.workspaceId,
  })
  const libraryParams = compactScopeQuery({
    scopeKind: 'library',
    workspaceId: scopeContext.workspaceId,
    libraryId: scopeContext.libraryId,
  })

  const providersQuery = useQuery({
    ...queries.listAiProvidersOptions(),
    select: mapProviderList,
  })
  const modelsQuery = useQuery({
    ...queries.listAiModelsOptions({ query: modelParams }),
    select: mapModelList,
  })
  const localAccountsQuery = useQuery({
    ...queries.listAiAccountsOptions({ query: localParams }),
    select: (raw) => mapAccountList(raw, providersQuery.data ?? []),
  })
  const visibleAccountsQuery = useQuery({
    ...queries.listAiAccountsOptions({ query: visibleParams }),
    enabled: selectedScope !== 'instance',
    select: (raw) => mapAccountList(raw, providersQuery.data ?? []),
  })
  const instanceBindingsQuery = useQuery({
    ...queries.listAiLibraryBindingsOptions({ query: instanceParams }),
    select: mapBindingList,
  })
  const workspaceBindingsQuery = useQuery({
    ...queries.listAiLibraryBindingsOptions({ query: workspaceParams }),
    enabled: Boolean(scopeContext.workspaceId),
    select: mapBindingList,
  })
  const libraryBindingsQuery = useQuery({
    ...queries.listAiLibraryBindingsOptions({ query: libraryParams }),
    enabled: Boolean(scopeContext.workspaceId && scopeContext.libraryId),
    select: mapBindingList,
  })

  const localAccounts = localAccountsQuery.data ?? []
  const instanceBindings = instanceBindingsQuery.data ?? []
  const workspaceBindings = workspaceBindingsQuery.data ?? []
  const libraryBindings = libraryBindingsQuery.data ?? []
  const bindingsForScope = {
    instance: instanceBindings,
    workspace: workspaceBindings,
    library: libraryBindings,
  }[selectedScope]
  const requiredQueries = [
    providersQuery,
    modelsQuery,
    localAccountsQuery,
    instanceBindingsQuery,
    ...(selectedScope === 'instance' ? [] : [visibleAccountsQuery]),
    ...(selectedScope === 'workspace' ? [workspaceBindingsQuery] : []),
    ...(selectedScope === 'library' ? [workspaceBindingsQuery, libraryBindingsQuery] : []),
  ]

  return {
    providers: providersQuery.data ?? [],
    models: modelsQuery.data ?? [],
    localAccounts,
    availableAccounts:
      selectedScope === 'instance' ? localAccounts : (visibleAccountsQuery.data ?? []),
    instanceBindings,
    workspaceBindings,
    libraryBindings,
    bindingsForScope,
    localParams,
    isLoading: requiredQueries.some((query) => query.isPending && query.fetchStatus !== 'idle'),
    error: requiredQueries.find((query) => query.error)?.error ?? null,
    refetch: async () => {
      await Promise.all(requiredQueries.map((query) => query.refetch()))
    },
  }
}

export function useProviderDetail(providerId: string | null) {
  return useQuery({
    ...queries.getAiProviderOptions({ path: { providerId: providerId ?? '' } }),
    enabled: Boolean(providerId),
    select: (raw) => mapProviderList([raw])[0],
  })
}

export function useModelDetail(modelId: string | null) {
  return useQuery({
    ...queries.getAiModelOptions({ path: { modelId: modelId ?? '' } }),
    enabled: Boolean(modelId),
    select: (raw) => mapModelList([raw])[0],
  })
}

export function useAccountDetail(
  accountId: string | null,
  providerList: ReturnType<typeof mapProviderList>,
) {
  return useQuery({
    ...queries.getAiAccountOptions({ path: { accountId: accountId ?? '' } }),
    enabled: Boolean(accountId),
    staleTime: 0,
    refetchOnMount: 'always',
    select: (raw) => mapAccountList([raw], providerList)[0],
  })
}

export function useBindingDetail(bindingId: string | null) {
  return useQuery({
    ...queries.getAiLibraryBindingOptions({ path: { bindingId: bindingId ?? '' } }),
    enabled: Boolean(bindingId),
    select: (raw) => mapBindingList([raw])[0],
  })
}

/**
 * The models a provider reports for one account's credentials.
 *
 * This is an outbound call to the provider, so it is requested only while a
 * binding dialog is actually open on that account, and a failure is surfaced
 * rather than retried: a router with a rejected key would otherwise retry
 * silently behind an empty dropdown, which is the exact confusion this listing
 * exists to remove.
 */
export function useAvailableProviderModels(accountId: string | null) {
  return useQuery({
    ...queries.listAvailableProviderModelsOptions({ path: { account_id: accountId ?? '' } }),
    enabled: Boolean(accountId),
    retry: false,
    staleTime: 5 * 60 * 1000,
  })
}

export function useBindingValidations(bindingId: string | null) {
  return useQuery({
    ...queries.listAiBindingValidationsOptions({ path: { bindingId: bindingId ?? '' } }),
    enabled: Boolean(bindingId),
    select: (items) => items.slice().sort((a, b) => b.checkedAt.localeCompare(a.checkedAt)),
  })
}

export function useBindingValidation(bindingId: string | null, validationId: string | null) {
  return useQuery({
    ...queries.getAiBindingValidationOptions({
      path: { bindingId: bindingId ?? '', validationId: validationId ?? '' },
    }),
    enabled: Boolean(bindingId && validationId),
    refetchInterval: (query) => (query.state.data?.validationState === 'pending' ? 1_200 : false),
  })
}

export function useAiAdminMutations() {
  const invalidate = useInvalidateOperations()
  const providerCreate = useMutation({
    mutationFn: (body: CreateProviderRequest) => adminApi.createProvider(body),
    onSuccess: () => invalidate('listAiProviders'),
  })
  const providerUpdate = useMutation({
    mutationFn: ({ id, body }: { id: string; body: UpdateProviderRequest }) =>
      adminApi.updateProvider(id, body),
    onSuccess: () => invalidate('listAiProviders'),
  })
  const providerDelete = useMutation({
    mutationFn: (id: string) => adminApi.deleteProvider(id),
    onSuccess: () => invalidate('listAiProviders'),
  })
  const modelCreate = useMutation({
    mutationFn: (body: CreateModelRequest) => adminApi.createModel(body),
    onSuccess: () => invalidate('listAiModels'),
  })
  const modelUpdate = useMutation({
    mutationFn: ({ id, body }: { id: string; body: UpdateModelRequest }) =>
      adminApi.updateModel(id, body),
    onSuccess: () => invalidate('listAiModels'),
  })
  const modelDelete = useMutation({
    mutationFn: (id: string) => adminApi.deleteModel(id),
    onSuccess: () => invalidate('listAiModels'),
  })
  const accountCreate = useMutation({
    mutationFn: (body: CreateAccountRequest) => adminApi.createAccount(body),
    onSuccess: () => invalidate('listAiAccounts', 'getAiAccount', 'listAiModels', 'getAiModel'),
  })
  const accountUpdate = useMutation({
    mutationFn: ({ id, body }: { id: string; body: UpdateAccountRequest }) =>
      adminApi.updateAccount(id, body),
    onSuccess: () => invalidate('listAiAccounts', 'getAiAccount', 'listAiModels', 'getAiModel'),
  })
  const accountDelete = useMutation({
    mutationFn: (id: string) => adminApi.deleteAccount(id),
    onSuccess: () => invalidate('listAiAccounts', 'getAiAccount', 'listAiModels', 'getAiModel'),
  })
  const bindingCreate = useMutation({
    mutationFn: (body: CreateBindingRequest) => adminApi.createBinding(body),
    onSuccess: () => invalidate('listAiLibraryBindings'),
  })
  const bindingUpdate = useMutation({
    mutationFn: ({ id, body }: { id: string; body: UpdateBindingRequest }) =>
      adminApi.updateBinding(id, body),
    onSuccess: () => invalidate('listAiLibraryBindings'),
  })
  const bindingDelete = useMutation({
    mutationFn: (id: string) => adminApi.deleteBinding(id),
    onSuccess: () => invalidate('listAiLibraryBindings'),
  })
  const bindingValidate = useMutation({
    mutationFn: (bindingId: string) => adminApi.validateBinding(bindingId),
    onSuccess: (_result: BindingValidationResponse) => invalidate('listAiBindingValidations'),
  })

  return {
    providerCreate,
    providerUpdate,
    providerDelete,
    modelCreate,
    modelUpdate,
    modelDelete,
    accountCreate,
    accountUpdate,
    accountDelete,
    bindingCreate,
    bindingUpdate,
    bindingDelete,
    bindingValidate,
  }
}
