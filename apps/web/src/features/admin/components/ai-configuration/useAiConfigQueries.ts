import { useCallback, useMemo } from 'react';
import { useQuery, useQueryClient } from '@tanstack/react-query';

import { ADMIN_MODEL_CATALOG_QUERY_KEY, adminApi, adminModelCatalogOptions } from '@/shared/api';
import type {
  AIBindingAssignment,
  AICredential,
  AIModelOption,
  AIProvider,
  AIScopeKind,
  ModelPreset,
} from '@/shared/types';
import {
  mapBindingList,
  mapCredentialList,
  mapModelList,
  mapPresetList,
  mapProviderList,
} from '@/features/admin/model/aiAdapter';
import {
  localScopeQuery,
  compactScopeQuery,
  modelCatalogScopeQuery,
  visibleScopeQuery,
  type AiConfigDataState,
  type AiConfigSection,
  type AiScopeContext,
} from '@/features/admin/model/aiConfig';

type UseAiConfigQueriesArgs = {
  active: boolean;
  activeSection: AiConfigSection;
  selectedScope: AIScopeKind;
  workspaceId?: string | undefined;
  libraryId?: string | undefined;
};

type BindingsData = {
  ready: true;
};

type AiScopeQuery = ReturnType<typeof compactScopeQuery>;

const ADMIN_AI_PROVIDERS_QUERY_KEY = ['admin', 'ai', 'providers'] as const;
const ADMIN_AI_CREDENTIALS_QUERY_KEY = ['admin', 'ai', 'credentials'] as const;
const ADMIN_AI_PRESETS_QUERY_KEY = ['admin', 'ai', 'presets'] as const;
export const ADMIN_AI_BINDINGS_QUERY_KEY = ['admin', 'ai', 'bindings'] as const;
export const ADMIN_AI_PRICES_QUERY_KEY = ['admin', 'ai', 'prices'] as const;

function adminAiCredentialsQueryKey(params: AiScopeQuery) {
  return [...ADMIN_AI_CREDENTIALS_QUERY_KEY, params] as const;
}

function adminAiPresetsQueryKey(params: AiScopeQuery) {
  return [...ADMIN_AI_PRESETS_QUERY_KEY, params] as const;
}

export function adminAiBindingsQueryKey(params: AiScopeQuery) {
  return [...ADMIN_AI_BINDINGS_QUERY_KEY, params] as const;
}

function stateFor<T>(isLoading: boolean, error: unknown, data: T | undefined): AiConfigDataState<T> {
  return { isLoading, error, data };
}

function queryIsLoading(query: { isLoading: boolean; fetchStatus: string }) {
  return query.isLoading && query.fetchStatus !== 'idle';
}

export function useAiConfigQueries({
  active,
  activeSection,
  selectedScope,
  workspaceId,
  libraryId,
}: UseAiConfigQueriesArgs) {
  const queryClient = useQueryClient();
  const scopeContext = useMemo<AiScopeContext>(
    () => ({ workspaceId, libraryId }),
    [libraryId, workspaceId],
  );
  const localScopeParams = useMemo(
    () => localScopeQuery(selectedScope, scopeContext),
    [scopeContext, selectedScope],
  );
  const visibleScopeParams = useMemo(
    () => visibleScopeQuery(selectedScope, scopeContext),
    [scopeContext, selectedScope],
  );
  const localQueryParams = useMemo(
    () => compactScopeQuery(localScopeParams.query),
    [localScopeParams],
  );
  const visibleQueryParams = useMemo(
    () => compactScopeQuery(visibleScopeParams.query),
    [visibleScopeParams],
  );
  const visibleModelParams = useMemo(
    () => modelCatalogScopeQuery(visibleScopeParams.query),
    [visibleScopeParams],
  );
  const workspaceBindingParams = useMemo(
    () => compactScopeQuery({ scopeKind: 'workspace', workspaceId }),
    [workspaceId],
  );
  const libraryBindingParams = useMemo(
    () => compactScopeQuery({ scopeKind: 'library', workspaceId, libraryId }),
    [libraryId, workspaceId],
  );

  const modelsEnabled = active;
  const localCredentialsEnabled = active;
  const localPresetsEnabled = active;
  const bindingsEnabled = active;
  const pricesEnabled = active;

  const providersQuery = useQuery({
    queryKey: ADMIN_AI_PROVIDERS_QUERY_KEY,
    queryFn: () => adminApi.listProviders(),
    enabled: active,
  });
  const pricesQuery = useQuery({
    queryKey: [...ADMIN_AI_PRICES_QUERY_KEY, workspaceId ?? null],
    queryFn: () => adminApi.listPrices(workspaceId ? { workspaceId } : {}),
    enabled: pricesEnabled,
  });
  const modelsQuery = useQuery({
    ...adminModelCatalogOptions(visibleModelParams),
    enabled: modelsEnabled,
  });
  const localCredentialsQuery = useQuery({
    queryKey: adminAiCredentialsQueryKey(localQueryParams),
    queryFn: () => adminApi.listCredentials(localQueryParams),
    enabled: localCredentialsEnabled,
  });
  const localPresetsQuery = useQuery({
    queryKey: adminAiPresetsQueryKey(localQueryParams),
    queryFn: () => adminApi.listModelPresets(localQueryParams),
    enabled: localPresetsEnabled,
  });
  const visibleCredentialsQuery = useQuery({
    queryKey: adminAiCredentialsQueryKey(visibleQueryParams),
    queryFn: () => adminApi.listCredentials(visibleQueryParams),
    enabled: bindingsEnabled && selectedScope !== 'instance',
  });
  const visiblePresetsQuery = useQuery({
    queryKey: adminAiPresetsQueryKey(visibleQueryParams),
    queryFn: () => adminApi.listModelPresets(visibleQueryParams),
    enabled: bindingsEnabled && selectedScope !== 'instance',
  });
  const instanceBindingsQuery = useQuery({
    queryKey: adminAiBindingsQueryKey({ scopeKind: 'instance' }),
    queryFn: () => adminApi.listBindings({ scopeKind: 'instance' }),
    enabled: bindingsEnabled,
  });
  const workspaceBindingsQuery = useQuery({
    queryKey: adminAiBindingsQueryKey(workspaceBindingParams),
    queryFn: () => adminApi.listBindings({ scopeKind: 'workspace', ...workspaceBindingParams }),
    enabled: bindingsEnabled && Boolean(workspaceId),
  });
  const libraryBindingsQuery = useQuery({
    queryKey: adminAiBindingsQueryKey(libraryBindingParams),
    queryFn: () => adminApi.listBindings({ scopeKind: 'library', ...libraryBindingParams }),
    enabled: bindingsEnabled && Boolean(workspaceId) && Boolean(libraryId),
  });

  const providers = useMemo<AIProvider[]>(
    () => mapProviderList(providersQuery.data),
    [providersQuery.data],
  );
  const models = useMemo<AIModelOption[]>(
    () => mapModelList(modelsQuery.data),
    [modelsQuery.data],
  );
  const localCredentials = useMemo<AICredential[]>(
    () => mapCredentialList(localCredentialsQuery.data, providers),
    [localCredentialsQuery.data, providers],
  );
  const localPresets = useMemo<ModelPreset[]>(
    () => mapPresetList(localPresetsQuery.data, providers, models),
    [localPresetsQuery.data, models, providers],
  );
  const availableCredentials = useMemo<AICredential[]>(() => {
    if (selectedScope === 'instance') {
      return localCredentials;
    }
    return mapCredentialList(visibleCredentialsQuery.data, providers);
  }, [localCredentials, providers, selectedScope, visibleCredentialsQuery.data]);
  const availablePresets = useMemo<ModelPreset[]>(() => {
    if (selectedScope === 'instance') {
      return localPresets;
    }
    return mapPresetList(visiblePresetsQuery.data, providers, models);
  }, [localPresets, models, providers, selectedScope, visiblePresetsQuery.data]);
  const instanceBindings = useMemo<AIBindingAssignment[]>(
    () => mapBindingList(instanceBindingsQuery.data),
    [instanceBindingsQuery.data],
  );
  const workspaceBindings = useMemo<AIBindingAssignment[]>(
    () => mapBindingList(workspaceBindingsQuery.data),
    [workspaceBindingsQuery.data],
  );
  const libraryBindings = useMemo<AIBindingAssignment[]>(
    () => mapBindingList(libraryBindingsQuery.data),
    [libraryBindingsQuery.data],
  );
  const bindingsForScope =
    selectedScope === 'instance'
      ? instanceBindings
      : selectedScope === 'workspace'
        ? workspaceBindings
        : libraryBindings;
  const modelById = useMemo(
    () => new Map(models.map(model => [model.id, model])),
    [models],
  );
  const priceRuleCount = Array.isArray(pricesQuery.data) ? pricesQuery.data.length : 0;

  const invalidateAll = useCallback(() => {
    void queryClient.invalidateQueries({ queryKey: ADMIN_AI_PROVIDERS_QUERY_KEY });
    void queryClient.invalidateQueries({ queryKey: ADMIN_AI_CREDENTIALS_QUERY_KEY });
    void queryClient.invalidateQueries({ queryKey: ADMIN_AI_PRESETS_QUERY_KEY });
    void queryClient.invalidateQueries({ queryKey: ADMIN_AI_BINDINGS_QUERY_KEY });
    void queryClient.invalidateQueries({ queryKey: ADMIN_AI_PRICES_QUERY_KEY });
    void queryClient.invalidateQueries({ queryKey: ADMIN_MODEL_CATALOG_QUERY_KEY });
  }, [queryClient]);

  const bindingsQueries = [
    providersQuery,
    modelsQuery,
    localCredentialsQuery,
    localPresetsQuery,
    visibleCredentialsQuery,
    visiblePresetsQuery,
    instanceBindingsQuery,
    workspaceBindingsQuery,
    libraryBindingsQuery,
  ];
  const bindingsLoading = bindingsEnabled && bindingsQueries.some(queryIsLoading);
  const bindingsError = bindingsQueries.find(query => query.error)?.error ?? null;

  return {
    scopeContext,
    localScopeParams,
    visibleScopeParams,
    providers,
    models,
    localCredentials,
    localPresets,
    availableCredentials,
    availablePresets,
    instanceBindings,
    workspaceBindings,
    libraryBindings,
    bindingsForScope,
    modelById,
    priceRuleCount,
    invalidateAll,
    providersState: stateFor(
      active && activeSection === 'providers' && queryIsLoading(providersQuery),
      providersQuery.error,
      providersQuery.data === undefined ? undefined : providers,
    ),
    modelsState: stateFor(
      active && activeSection === 'models' && (
        queryIsLoading(providersQuery) || queryIsLoading(modelsQuery)
      ),
      providersQuery.error ?? modelsQuery.error,
      providersQuery.data === undefined || modelsQuery.data === undefined ? undefined : models,
    ),
    credentialsState: stateFor(
      active && activeSection === 'credentials' && (
        queryIsLoading(providersQuery) || queryIsLoading(localCredentialsQuery)
      ),
      providersQuery.error ?? localCredentialsQuery.error,
      providersQuery.data === undefined || localCredentialsQuery.data === undefined
        ? undefined
        : localCredentials,
    ),
    presetsState: stateFor(
      active && activeSection === 'presets' && (
        queryIsLoading(providersQuery) || queryIsLoading(modelsQuery) || queryIsLoading(localPresetsQuery)
      ),
      providersQuery.error ?? modelsQuery.error ?? localPresetsQuery.error,
      providersQuery.data === undefined || modelsQuery.data === undefined || localPresetsQuery.data === undefined
        ? undefined
        : localPresets,
    ),
    bindingsState: stateFor<BindingsData>(
      bindingsLoading,
      bindingsError,
      bindingsLoading ? undefined : { ready: true },
    ),
  };
}

export type AiConfigQueries = ReturnType<typeof useAiConfigQueries>;
