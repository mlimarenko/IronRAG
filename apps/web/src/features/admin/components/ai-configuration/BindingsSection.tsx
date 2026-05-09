import { useEffect, useMemo, useState } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useTranslation } from 'react-i18next';
import { toast } from 'sonner';

import { adminApi, adminModelCatalogOptions } from '@/shared/api';
import type { AiBindingAssignmentResponse } from '@/shared/api/generated';
import { DataState } from '@/shared/components/DataState';
import { Badge } from '@/shared/components/ui/badge';
import { errorMessage } from '@/shared/lib/errorMessage';
import { shouldRefreshCredentialModels } from '@/shared/lib/ai-provider';
import type {
  AIBindingAssignment,
  AICredential,
  AIModelOption,
  AIPurpose,
  AIScopeKind,
  ModelPreset,
} from '@/shared/types';
import { mapModelList } from '@/features/admin/model/aiAdapter';
import {
  PURPOSE_ORDER,
  REQUIRED_RUNTIME_PURPOSE_ORDER,
  compactScopeQuery,
  localScopeQuery,
  modelCatalogScopeQuery,
  resolveBindingForPurpose,
  suggestBindingSelection,
  visibleScopeQuery,
  type AiConfigDataState,
  type AiScopeContext,
  type CredentialModelLoadState,
} from '@/features/admin/model/aiConfig';
import { BindingPurposeCard } from './BindingPurposeCard';
import { adminAiBindingsQueryKey } from './useAiConfigQueries';

type BindingsSectionProps = {
  selectedScope: AIScopeKind;
  scopeContext: AiScopeContext;
  bindingsState: AiConfigDataState<{ ready: true }>;
  availableCredentials: AICredential[];
  availablePresets: ModelPreset[];
  localCredentials: AICredential[];
  localPresets: ModelPreset[];
  bindingsForScope: AIBindingAssignment[];
  instanceBindings: AIBindingAssignment[];
  workspaceBindings: AIBindingAssignment[];
  modelById: Map<string, AIModelOption>;
  invalidateAll: () => void;
};

type BindingMutationContext = {
  previousBindings: AiBindingAssignmentResponse[] | undefined;
};

type BindingScopeQuery = ReturnType<typeof compactScopeQuery>;

type BindingSaveVariables = {
  bindingId: string | null;
  credentialId: string;
  optimisticId: string;
  presetId: string;
  purpose: AIPurpose;
  scopeKind: AIScopeKind;
  scopeQuery: BindingScopeQuery;
};

type BindingResetVariables = {
  bindingId: string;
  purpose: AIPurpose;
  scopeQuery: BindingScopeQuery;
};

function buildOptimisticBinding({
  bindingId,
  credentialId,
  optimisticId,
  presetId,
  purpose,
  scopeKind,
  scopeQuery,
}: BindingSaveVariables): AiBindingAssignmentResponse {
  return {
    bindingPurpose: purpose,
    bindingState: 'active',
    id: bindingId ?? optimisticId,
    modelPresetId: presetId,
    providerCredentialId: credentialId,
    scopeKind,
    ...(scopeQuery.workspaceId ? { workspaceId: scopeQuery.workspaceId } : {}),
    ...(scopeQuery.libraryId ? { libraryId: scopeQuery.libraryId } : {}),
  };
}

function applyOptimisticBinding(
  current: AiBindingAssignmentResponse[] | undefined,
  binding: AiBindingAssignmentResponse,
): AiBindingAssignmentResponse[] {
  return [
    ...(current ?? []).filter(
      (entry) =>
        entry.id !== binding.id &&
        entry.bindingPurpose !== binding.bindingPurpose,
    ),
    binding,
  ];
}

export function BindingsSection({
  selectedScope,
  scopeContext,
  bindingsState,
  availableCredentials,
  availablePresets,
  localCredentials,
  localPresets,
  bindingsForScope,
  instanceBindings,
  workspaceBindings,
  modelById,
  invalidateAll,
}: BindingsSectionProps) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const [editingPurpose, setEditingPurpose] = useState<AIPurpose | null>(null);
  const [bindingCredentialId, setBindingCredentialId] = useState('');
  const [bindingPresetId, setBindingPresetId] = useState('');
  const localScopeParams = useMemo(
    () => compactScopeQuery(localScopeQuery(selectedScope, scopeContext).query),
    [scopeContext, selectedScope],
  );

  const selectedBindingCredential = useMemo(
    () =>
      bindingCredentialId
        ? availableCredentials.find(entry => entry.id === bindingCredentialId) ?? null
        : null,
    [availableCredentials, bindingCredentialId],
  );
  const selectedBindingCredentialModelsQueryParams = {
    ...(selectedBindingCredential ? {
      providerCatalogId: selectedBindingCredential.providerId,
      credentialId: selectedBindingCredential.id,
    } : {}),
    ...modelCatalogScopeQuery(visibleScopeQuery(selectedScope, scopeContext).query),
  };
  const selectedBindingCredentialModelsQuery = useQuery({
    ...adminModelCatalogOptions(selectedBindingCredentialModelsQueryParams),
    enabled: Boolean(editingPurpose) && shouldRefreshCredentialModels(selectedBindingCredential?.provider),
  });
  const selectedBindingCredentialModels = useMemo<AIModelOption[] | null>(
    () =>
      selectedBindingCredentialModelsQuery.data
        ? mapModelList(selectedBindingCredentialModelsQuery.data)
        : null,
    [selectedBindingCredentialModelsQuery.data],
  );
  const modelsByCredentialId = useMemo<Record<string, AIModelOption[]>>(() => {
    if (!selectedBindingCredential || !selectedBindingCredentialModels) {
      return {};
    }
    return { [selectedBindingCredential.id]: selectedBindingCredentialModels };
  }, [selectedBindingCredential, selectedBindingCredentialModels]);
  const selectedBindingCredentialLoadState: CredentialModelLoadState | undefined =
    selectedBindingCredentialModelsQuery.isLoading || selectedBindingCredentialModelsQuery.isFetching
      ? 'loading'
      : selectedBindingCredentialModelsQuery.error
        ? 'failed'
        : selectedBindingCredentialModels
          ? 'ready'
          : undefined;

  useEffect(() => {
    if (selectedBindingCredentialModelsQuery.error) {
      toast.error(t('admin.aiPanel.messages.credentialModelRefreshFailed'));
    }
  }, [selectedBindingCredentialModelsQuery.error, t]);

  const resolveBinding = (purpose: AIPurpose) =>
    resolveBindingForPurpose({
      purpose,
      selectedScope,
      bindingsForScope,
      instanceBindings,
      workspaceBindings,
    });
  const openBindingEditor = (purpose: AIPurpose) => {
    const resolved = resolveBinding(purpose);
    const suggestion = suggestBindingSelection({
      purpose,
      availableCredentials,
      availablePresets,
      modelById,
      preferredCredentialId: resolved.localBinding?.credentialId ?? resolved.effectiveBinding?.credentialId,
      preferredPresetId: resolved.localBinding?.presetId ?? resolved.effectiveBinding?.presetId,
    });
    setEditingPurpose(purpose);
    setBindingCredentialId(suggestion.credentialId);
    setBindingPresetId(suggestion.presetId);
  };

  const saveBindingMutation = useMutation<
    AiBindingAssignmentResponse,
    unknown,
    BindingSaveVariables,
    BindingMutationContext
  >({
    mutationKey: ['admin', 'ai', 'bindings', 'save'],
    scope: { id: `admin:ai:bindings:${selectedScope}:${scopeContext.workspaceId ?? 'instance'}:${scopeContext.libraryId ?? 'none'}` },
    mutationFn: (variables) =>
      variables.bindingId
        ? adminApi.updateBinding(variables.bindingId, {
            providerCredentialId: variables.credentialId,
            modelPresetId: variables.presetId,
            bindingState: 'active',
          })
        : adminApi.createBinding({
            ...variables.scopeQuery,
            scopeKind: variables.scopeKind,
            bindingPurpose: variables.purpose,
            providerCredentialId: variables.credentialId,
            modelPresetId: variables.presetId,
          }),
    onMutate: async (variables) => {
      const queryKey = adminAiBindingsQueryKey(variables.scopeQuery);
      await queryClient.cancelQueries({ queryKey });
      const previousBindings =
        queryClient.getQueryData<AiBindingAssignmentResponse[]>(queryKey);
      queryClient.setQueryData<AiBindingAssignmentResponse[]>(
        queryKey,
        (current) =>
          applyOptimisticBinding(
            current,
            buildOptimisticBinding(variables),
          ),
      );
      setEditingPurpose(null);
      setBindingCredentialId('');
      setBindingPresetId('');
      return { previousBindings };
    },
    onSuccess: (binding, variables) => {
      queryClient.setQueryData<AiBindingAssignmentResponse[]>(
        adminAiBindingsQueryKey(variables.scopeQuery),
        (current = []) =>
          current.map((entry) =>
            entry.id === variables.optimisticId || entry.id === binding.id
              ? binding
              : entry,
          ),
      );
      toast.success(t('admin.aiPanel.messages.bindingSaved'));
    },
    onError: (err, variables, context) => {
      if (context) {
        queryClient.setQueryData(
          adminAiBindingsQueryKey(variables.scopeQuery),
          context.previousBindings,
        );
      }
      toast.error(
        t('admin.aiPanel.messages.bindingRollbackFailed', {
          error: errorMessage(err, t('admin.aiPanel.messages.bindingSaveFailed')),
        }),
      );
    },
    onSettled: () => {
      invalidateAll();
    },
  });

  const resetBindingMutation = useMutation<
    void,
    unknown,
    BindingResetVariables,
    BindingMutationContext
  >({
    mutationKey: ['admin', 'ai', 'bindings', 'reset'],
    scope: { id: `admin:ai:bindings:${selectedScope}:${scopeContext.workspaceId ?? 'instance'}:${scopeContext.libraryId ?? 'none'}` },
    mutationFn: ({ bindingId }) => adminApi.deleteBinding(bindingId),
    onMutate: async (variables) => {
      const queryKey = adminAiBindingsQueryKey(variables.scopeQuery);
      await queryClient.cancelQueries({ queryKey });
      const previousBindings =
        queryClient.getQueryData<AiBindingAssignmentResponse[]>(queryKey);
      queryClient.setQueryData<AiBindingAssignmentResponse[]>(
        queryKey,
        (current = []) =>
          current.filter(
            (entry) =>
              entry.id !== variables.bindingId &&
              entry.bindingPurpose !== variables.purpose,
          ),
      );
      setEditingPurpose(null);
      setBindingCredentialId('');
      setBindingPresetId('');
      return { previousBindings };
    },
    onSuccess: () => {
      toast.success(t('admin.aiPanel.messages.overrideRemoved'));
    },
    onError: (err, variables, context) => {
      if (context) {
        queryClient.setQueryData(
          adminAiBindingsQueryKey(variables.scopeQuery),
          context.previousBindings,
        );
      }
      toast.error(
        t('admin.aiPanel.messages.bindingRollbackFailed', {
          error: errorMessage(err, t('admin.aiPanel.messages.overrideRemoveFailed')),
        }),
      );
    },
    onSettled: () => {
      invalidateAll();
    },
  });

  const saveBinding = async (purpose: AIPurpose) => {
    const resolved = resolveBinding(purpose);
    if (!bindingCredentialId || !bindingPresetId) {
      return;
    }
    saveBindingMutation.mutate({
      bindingId: resolved.localBinding?.id ?? null,
      credentialId: bindingCredentialId,
      optimisticId: `optimistic-binding-${selectedScope}-${purpose}`,
      presetId: bindingPresetId,
      purpose,
      scopeKind: selectedScope,
      scopeQuery: localScopeParams,
    });
  };
  const resetBinding = async (purpose: AIPurpose) => {
    const resolved = resolveBinding(purpose);
    if (!resolved.localBinding || selectedScope === 'instance') {
      return;
    }
    resetBindingMutation.mutate({
      bindingId: resolved.localBinding.id,
      purpose,
      scopeQuery: localScopeParams,
    });
  };

  const showMissingInstanceNotice =
    selectedScope !== 'instance'
    && instanceBindings.length === 0
    && localCredentials.length + localPresets.length + bindingsForScope.length > 0;
  const optionalPurposes = PURPOSE_ORDER.filter(purpose => !REQUIRED_RUNTIME_PURPOSE_ORDER.includes(purpose));
  const configuredRequiredBindings = REQUIRED_RUNTIME_PURPOSE_ORDER.filter(purpose => resolveBinding(purpose).effectiveBinding).length;
  const renderPurpose = (purpose: AIPurpose) => {
    const resolved = resolveBinding(purpose);
    return (
      <BindingPurposeCard
        key={purpose}
        purpose={purpose}
        selectedScope={selectedScope}
        resolved={resolved}
        availableCredentials={availableCredentials}
        availablePresets={availablePresets}
        modelById={modelById}
        modelsByCredentialId={modelsByCredentialId}
        selectedBindingCredential={selectedBindingCredential}
        selectedBindingCredentialLoadState={selectedBindingCredentialLoadState}
        editing={editingPurpose === purpose}
        bindingCredentialId={bindingCredentialId}
        bindingPresetId={bindingPresetId}
        bindingSaving={saveBindingMutation.isPending || resetBindingMutation.isPending}
        onCredentialChange={value => {
          setBindingCredentialId(value);
          setBindingPresetId('');
        }}
        onPresetChange={setBindingPresetId}
        onOpen={() => openBindingEditor(purpose)}
        onCancel={() => setEditingPurpose(null)}
        onSave={() => void saveBinding(purpose)}
        onReset={() => void resetBinding(purpose)}
      />
    );
  };

  return (
    <DataState query={bindingsState}>
      {() => (
        <div className="space-y-3">
          {showMissingInstanceNotice && (
            <div className="rounded-md border border-status-warning/20 bg-status-warning/5 p-3 text-sm text-status-warning">
              {t('admin.aiPanel.notices.missingInstanceBaseline')}
            </div>
          )}
          <div className="flex flex-wrap items-center gap-2">
            <h3 className="text-sm font-bold tracking-tight">{t('admin.aiPanel.sections.requiredBindingsTitle')}</h3>
            <Badge variant="outline">{configuredRequiredBindings}/{REQUIRED_RUNTIME_PURPOSE_ORDER.length}</Badge>
          </div>

          <div className="overflow-hidden rounded-md border border-border/70 bg-card">
            {REQUIRED_RUNTIME_PURPOSE_ORDER.map(renderPurpose)}
          </div>

          <details className="overflow-hidden rounded-md border border-border/70 bg-card">
            <summary className="flex cursor-pointer list-none items-center justify-between gap-3 px-3 py-2 text-sm font-semibold text-muted-foreground">
              <span>{t('admin.aiPanel.sections.optionalBindingsTitle')}</span>
              <Badge variant="outline">{optionalPurposes.length}</Badge>
            </summary>
            <div className="border-t border-border/70">
              {optionalPurposes.map(renderPurpose)}
            </div>
          </details>
        </div>
      )}
    </DataState>
  );
}
