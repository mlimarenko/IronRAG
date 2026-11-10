import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'

import { adminApi, queries, retryContractViolation, validateQueryResponse } from '@/shared/api'
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

  const providerOptions = queries.listAiProvidersOptions()
  const providersQuery = useQuery({
    ...providerOptions,
    queryFn: validateQueryResponse(providerOptions.queryFn, mapProviderList),
    retry: retryContractViolation,
    select: mapProviderList,
  })
  const modelOptions = queries.listAiModelsOptions({ query: modelParams })
  const modelsQuery = useQuery({
    ...modelOptions,
    queryFn: validateQueryResponse(modelOptions.queryFn, mapModelList),
    retry: retryContractViolation,
    select: mapModelList,
  })
  const localAccountOptions = queries.listAiAccountsOptions({ query: localParams })
  const localAccountsQuery = useQuery({
    ...localAccountOptions,
    queryFn: validateQueryResponse(localAccountOptions.queryFn, (raw) =>
      mapAccountList(raw, providersQuery.data ?? []),
    ),
    retry: retryContractViolation,
    select: (raw) => mapAccountList(raw, providersQuery.data ?? []),
  })
  const visibleAccountOptions = queries.listAiAccountsOptions({ query: visibleParams })
  const visibleAccountsQuery = useQuery({
    ...visibleAccountOptions,
    queryFn: validateQueryResponse(visibleAccountOptions.queryFn, (raw) =>
      mapAccountList(raw, providersQuery.data ?? []),
    ),
    enabled: selectedScope !== 'instance',
    retry: retryContractViolation,
    select: (raw) => mapAccountList(raw, providersQuery.data ?? []),
  })
  const instanceBindingOptions = queries.listAiLibraryBindingsOptions({ query: instanceParams })
  const instanceBindingsQuery = useQuery({
    ...instanceBindingOptions,
    queryFn: validateQueryResponse(instanceBindingOptions.queryFn, mapBindingList),
    retry: retryContractViolation,
    select: mapBindingList,
  })
  const workspaceBindingOptions = queries.listAiLibraryBindingsOptions({ query: workspaceParams })
  const workspaceBindingsQuery = useQuery({
    ...workspaceBindingOptions,
    queryFn: validateQueryResponse(workspaceBindingOptions.queryFn, mapBindingList),
    enabled: Boolean(scopeContext.workspaceId),
    retry: retryContractViolation,
    select: mapBindingList,
  })
  const libraryBindingOptions = queries.listAiLibraryBindingsOptions({ query: libraryParams })
  const libraryBindingsQuery = useQuery({
    ...libraryBindingOptions,
    queryFn: validateQueryResponse(libraryBindingOptions.queryFn, mapBindingList),
    enabled: Boolean(scopeContext.workspaceId && scopeContext.libraryId),
    retry: retryContractViolation,
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
  const options = queries.getAiProviderOptions({ path: { providerId: providerId ?? '' } })
  return useQuery({
    ...options,
    queryFn: validateQueryResponse(options.queryFn, (raw) => mapProviderList([raw])[0]),
    enabled: Boolean(providerId),
    retry: retryContractViolation,
    select: (raw) => mapProviderList([raw])[0],
  })
}

export function useModelDetail(modelId: string | null) {
  const options = queries.getAiModelOptions({ path: { modelId: modelId ?? '' } })
  return useQuery({
    ...options,
    queryFn: validateQueryResponse(options.queryFn, (raw) => mapModelList([raw])[0]),
    enabled: Boolean(modelId),
    retry: retryContractViolation,
    select: (raw) => mapModelList([raw])[0],
  })
}

export function useAccountDetail(
  accountId: string | null,
  providerList: ReturnType<typeof mapProviderList>,
) {
  const options = queries.getAiAccountOptions({ path: { accountId: accountId ?? '' } })
  return useQuery({
    ...options,
    queryFn: validateQueryResponse(
      options.queryFn,
      (raw) => mapAccountList([raw], providerList)[0],
    ),
    enabled: Boolean(accountId),
    retry: retryContractViolation,
    staleTime: 0,
    refetchOnMount: 'always',
    select: (raw) => mapAccountList([raw], providerList)[0],
  })
}

export function useBindingDetail(bindingId: string | null) {
  const options = queries.getAiLibraryBindingOptions({ path: { bindingId: bindingId ?? '' } })
  return useQuery({
    ...options,
    queryFn: validateQueryResponse(options.queryFn, (raw) => mapBindingList([raw])[0]),
    enabled: Boolean(bindingId),
    retry: retryContractViolation,
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
