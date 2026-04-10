import { useCallback, useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { toast } from 'sonner';
import { AlertTriangle, Brain, KeyRound, Loader2, Settings2 } from 'lucide-react';

import { adminApi } from '@/api';
import { useApp } from '@/contexts/AppContext';
import { baseUrlForProviderInput } from '@/lib/ai-provider';
import type {
  AIBindingAssignment,
  AICredential,
  AIModelOption,
  AIProvider,
  AIPurpose,
  AIScopeKind,
  ModelPreset,
} from '@/types';
import { Button } from '@/components/ui/button';
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select';
import { Textarea } from '@/components/ui/textarea';

import { mapProvider, mapCredential, mapModelOption, mapPreset, mapBinding } from '@/lib/ai-mappers';

type BindingResolution = {
  localBinding: AIBindingAssignment | null;
  effectiveBinding: AIBindingAssignment | null;
  sourceKind: AIScopeKind | null;
};

const PURPOSE_ORDER: AIPurpose[] = ['extract_graph', 'embed_chunk', 'query_answer', 'vision'];

function purposeTranslationKey(value: AIPurpose) {
  return `admin.aiPanel.purposeLabels.${value}` as const;
}

function scopeTranslationKey(value: AIScopeKind) {
  return `admin.aiPanel.scopeLabels.${value}` as const;
}

function credentialStateTranslationKey(value: AICredential['state']) {
  return `admin.aiPanel.credentialStateLabels.${value}` as const;
}

function formatPresetLabel(preset: Pick<ModelPreset, 'presetName' | 'modelName'>) {
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

function badgeClass(value: 'ready' | 'warning' | 'failed') {
  return value === 'ready' ? 'status-ready' : value === 'failed' ? 'status-failed' : 'status-warning';
}

function parseNumber(value: string): number | null {
  const normalized = value.trim();
  if (!normalized) {
    return null;
  }
  const parsed = Number(normalized);
  return Number.isFinite(parsed) ? parsed : null;
}

function parseInteger(value: string): number | null {
  const normalized = value.trim();
  if (!normalized) {
    return null;
  }
  const parsed = Number.parseInt(normalized, 10);
  return Number.isFinite(parsed) ? parsed : null;
}

function isModelAvailableForCredential(
  model: AIModelOption | undefined,
  credential: AICredential | null | undefined,
  modelsByCredentialId: Record<string, AIModelOption[]>,
): boolean {
  if (!model || !credential || credential.providerKind !== 'ollama') {
    return true;
  }
  const discoveredModels = modelsByCredentialId[credential.id];
  if (!discoveredModels) {
    return model.availabilityState !== 'unavailable';
  }
  return discoveredModels.some(entry => entry.id === model.id);
}

function formatModelLabel(model: AIModelOption, providers: AIProvider[]) {
  const provider = providers.find(entry => entry.id === model.providerCatalogId);
  return provider ? `${provider.displayName} · ${model.modelName}` : model.modelName;
}

export default function AiConfigurationPanel() {
  const { t } = useTranslation();
  const { activeWorkspace, activeLibrary } = useApp();
  const purposeLabel = (value: AIPurpose) => t(purposeTranslationKey(value));
  const scopeLabel = (value: AIScopeKind) => t(scopeTranslationKey(value));
  const credentialStateLabel = (value: AICredential['state']) => t(credentialStateTranslationKey(value));
  const formatPurposeList = (purposes: AIPurpose[]) => (
    purposes.length > 0 ? purposes.map(purposeLabel).join(', ') : t('admin.aiPanel.none')
  );
  const [selectedScope, setSelectedScope] = useState<AIScopeKind>('instance');
  const autoSelectedScopeRef = useRef(false);
  const [providers, setProviders] = useState<AIProvider[]>([]);
  const [models, setModels] = useState<AIModelOption[]>([]);
  const [modelsByCredentialId, setModelsByCredentialId] = useState<Record<string, AIModelOption[]>>({});
  const [availableCredentials, setAvailableCredentials] = useState<AICredential[]>([]);
  const [availablePresets, setAvailablePresets] = useState<ModelPreset[]>([]);
  const [localCredentials, setLocalCredentials] = useState<AICredential[]>([]);
  const [localPresets, setLocalPresets] = useState<ModelPreset[]>([]);
  const [instanceBindings, setInstanceBindings] = useState<AIBindingAssignment[]>([]);
  const [workspaceBindings, setWorkspaceBindings] = useState<AIBindingAssignment[]>([]);
  const [libraryBindings, setLibraryBindings] = useState<AIBindingAssignment[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [reloadKey, setReloadKey] = useState(0);

  const [editingPurpose, setEditingPurpose] = useState<AIPurpose | null>(null);
  const [bindingCredentialId, setBindingCredentialId] = useState('');
  const [bindingPresetId, setBindingPresetId] = useState('');
  const [bindingSaving, setBindingSaving] = useState(false);

  const [credentialOpen, setCredentialOpen] = useState(false);
  const [editingCredential, setEditingCredential] = useState<AICredential | null>(null);
  const [credentialProviderId, setCredentialProviderId] = useState('');
  const [credentialLabel, setCredentialLabel] = useState('');
  const [credentialBaseUrl, setCredentialBaseUrl] = useState('');
  const [credentialApiKey, setCredentialApiKey] = useState('');
  const [credentialSaving, setCredentialSaving] = useState(false);

  const [presetOpen, setPresetOpen] = useState(false);
  const [editingPreset, setEditingPreset] = useState<ModelPreset | null>(null);
  const [presetName, setPresetName] = useState('');
  const [presetModelId, setPresetModelId] = useState('');
  const [presetSystemPrompt, setPresetSystemPrompt] = useState('');
  const [presetTemperature, setPresetTemperature] = useState('');
  const [presetTopP, setPresetTopP] = useState('');
  const [presetMaxTokens, setPresetMaxTokens] = useState('');
  const [presetSaving, setPresetSaving] = useState(false);

  const credentialSaveErrorMessage = (saveError: unknown) => {
    const message = String((saveError as { message?: string } | null)?.message ?? '');
    if (message.includes('provider credential validation failed')) {
      return t('admin.aiPanel.messages.credentialValidationFailed');
    }
    return t('admin.aiPanel.messages.credentialSaveFailed');
  };

  const selectedProvider = providers.find(entry => entry.id === credentialProviderId) ?? null;

  const scopeParams = useCallback((scopeKind: AIScopeKind) => {
    if (scopeKind === 'instance') {
      return { scopeKind };
    }
    if (scopeKind === 'workspace') {
      return { scopeKind, workspaceId: activeWorkspace?.id };
    }
    return { scopeKind, workspaceId: activeWorkspace?.id, libraryId: activeLibrary?.id };
  }, [activeLibrary?.id, activeWorkspace?.id]);

  const visibleParams = useCallback((scopeKind: AIScopeKind) => {
    if (scopeKind === 'instance') {
      return { scopeKind: 'instance' as const };
    }
    if (scopeKind === 'workspace') {
      return { workspaceId: activeWorkspace?.id };
    }
    return { workspaceId: activeWorkspace?.id, libraryId: activeLibrary?.id };
  }, [activeLibrary?.id, activeWorkspace?.id]);

  useEffect(() => {
    if (selectedScope === 'library' && !activeLibrary) {
      setSelectedScope(activeWorkspace ? 'workspace' : 'instance');
    }
    if (selectedScope === 'workspace' && !activeWorkspace) {
      setSelectedScope('instance');
    }
  }, [activeLibrary, activeWorkspace, selectedScope]);

  useEffect(() => {
    autoSelectedScopeRef.current = false;
  }, [activeLibrary?.id, activeWorkspace?.id]);

  useEffect(() => {
    let cancelled = false;
    const load = async () => {
      setLoading(true);
      setError(null);
      try {
        const visibleModelParams = visibleParams(selectedScope);
        const [providerRaw, modelRaw, localCredentialRaw, localPresetRaw, visibleCredentialRaw, visiblePresetRaw, instanceBindingRaw, workspaceBindingRaw, libraryBindingRaw] = await Promise.all([
          adminApi.listProviders(),
          adminApi.listModels(visibleModelParams),
          adminApi.listCredentials(scopeParams(selectedScope)),
          adminApi.listModelPresets(scopeParams(selectedScope)),
          adminApi.listCredentials(visibleParams(selectedScope)),
          adminApi.listModelPresets(visibleParams(selectedScope)),
          adminApi.listBindings({ scopeKind: 'instance' }),
          activeWorkspace?.id ? adminApi.listBindings({ scopeKind: 'workspace', workspaceId: activeWorkspace.id }) : Promise.resolve([]),
          activeLibrary?.id ? adminApi.listBindings({ scopeKind: 'library', workspaceId: activeWorkspace?.id, libraryId: activeLibrary.id }) : Promise.resolve([]),
        ]);
        if (cancelled) {
          return;
        }
        const providerList = (Array.isArray(providerRaw) ? providerRaw : []).map(mapProvider);
        const visibleCredentialList = (Array.isArray(visibleCredentialRaw) ? visibleCredentialRaw : []).map((entry: any) => mapCredential(entry, providerList));
        const modelList = (Array.isArray(modelRaw) ? modelRaw : []).map(mapModelOption);
        const ollamaCredentials = visibleCredentialList.filter(entry => entry.providerKind === 'ollama');
        const discoveredModelsByCredential = Object.fromEntries(await Promise.all(
          ollamaCredentials.map(async credential => {
            const resolved = await adminApi.listModels({
              providerCatalogId: credential.providerId,
              credentialId: credential.id,
              ...visibleModelParams,
            });
            return [credential.id, (Array.isArray(resolved) ? resolved : []).map(mapModelOption)];
          }),
        ));
        if (cancelled) {
          return;
        }
        setProviders(providerList);
        setModels(modelList);
        setModelsByCredentialId(discoveredModelsByCredential);
        setLocalCredentials((Array.isArray(localCredentialRaw) ? localCredentialRaw : []).map((entry: any) => mapCredential(entry, providerList)));
        setAvailableCredentials(visibleCredentialList);
        setLocalPresets((Array.isArray(localPresetRaw) ? localPresetRaw : []).map((entry: any) => mapPreset(entry, providerList, modelList)));
        setAvailablePresets((Array.isArray(visiblePresetRaw) ? visiblePresetRaw : []).map((entry: any) => mapPreset(entry, providerList, modelList)));
        setInstanceBindings((Array.isArray(instanceBindingRaw) ? instanceBindingRaw : []).map(mapBinding));
        setWorkspaceBindings((Array.isArray(workspaceBindingRaw) ? workspaceBindingRaw : []).map(mapBinding));
        setLibraryBindings((Array.isArray(libraryBindingRaw) ? libraryBindingRaw : []).map(mapBinding));
      } catch (loadError: any) {
        if (!cancelled) {
          setError(t('admin.aiPanel.messages.loadFailed'));
        }
      } finally {
        if (!cancelled) {
          setLoading(false);
        }
      }
    };
    void load();
    return () => {
      cancelled = true;
    };
  }, [activeLibrary?.id, activeWorkspace?.id, reloadKey, scopeParams, selectedScope, t, visibleParams]);

  useEffect(() => {
    if (loading || selectedScope !== 'instance' || autoSelectedScopeRef.current) {
      return;
    }

    const hasInstanceBaseline =
      instanceBindings.length > 0 || localCredentials.length > 0 || localPresets.length > 0;
    if (hasInstanceBaseline) {
      autoSelectedScopeRef.current = true;
      return;
    }

    const nextScope =
      activeLibrary && libraryBindings.length > 0
        ? 'library'
        : activeWorkspace && workspaceBindings.length > 0
          ? 'workspace'
          : null;

    if (!nextScope) {
      return;
    }

    autoSelectedScopeRef.current = true;
    setSelectedScope(nextScope);
  }, [
    activeLibrary,
    activeWorkspace,
    instanceBindings.length,
    libraryBindings.length,
    loading,
    localCredentials.length,
    localPresets.length,
    selectedScope,
    workspaceBindings.length,
  ]);

  const bindingsForScope = selectedScope === 'instance' ? instanceBindings : selectedScope === 'workspace' ? workspaceBindings : libraryBindings;
  const showMissingInstanceNotice =
    selectedScope !== 'instance'
    && instanceBindings.length === 0
    && localCredentials.length + localPresets.length + bindingsForScope.length > 0;
  const modelById = new Map(models.map(model => [model.id, model]));
  const selectableModels = models.filter(entry => {
    if (editingPreset && entry.id === presetModelId) {
      return true;
    }
    if (entry.providerCatalogId === '' || entry.providerCatalogId === undefined) {
      return false;
    }
    if (providers.find(provider => provider.id === entry.providerCatalogId)?.kind !== 'ollama') {
      return true;
    }
    return entry.availabilityState === 'available';
  });

  const resolveBinding = (purpose: AIPurpose): BindingResolution => {
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
  };

  const resetCredentialDialog = () => {
    setCredentialOpen(false);
    setEditingCredential(null);
    setCredentialProviderId('');
    setCredentialLabel('');
    setCredentialBaseUrl('');
    setCredentialApiKey('');
    setCredentialSaving(false);
  };

  const resetPresetDialog = () => {
    setPresetOpen(false);
    setEditingPreset(null);
    setPresetName('');
    setPresetModelId('');
    setPresetSystemPrompt('');
    setPresetTemperature('');
    setPresetTopP('');
    setPresetMaxTokens('');
    setPresetSaving(false);
  };

  const openBindingEditor = (purpose: AIPurpose) => {
    const resolved = resolveBinding(purpose);
    setEditingPurpose(purpose);
    setBindingCredentialId(resolved.localBinding?.credentialId ?? resolved.effectiveBinding?.credentialId ?? '');
    setBindingPresetId(resolved.localBinding?.presetId ?? resolved.effectiveBinding?.presetId ?? '');
  };

  const saveCredential = async () => {
    if (!selectedProvider || !credentialLabel.trim()) {
      return;
    }
    setCredentialSaving(true);
    try {
      if (editingCredential) {
        await adminApi.updateCredential(editingCredential.id, {
          label: credentialLabel.trim(),
          apiKey: credentialApiKey.trim() || undefined,
          baseUrl: credentialBaseUrl.trim() || undefined,
          credentialState: 'active',
        });
      } else {
        await adminApi.createCredential({
          ...scopeParams(selectedScope),
          providerCatalogId: credentialProviderId,
          label: credentialLabel.trim(),
          apiKey: credentialApiKey.trim() || undefined,
          baseUrl: credentialBaseUrl.trim() || undefined,
        });
      }
      resetCredentialDialog();
      setReloadKey(value => value + 1);
      toast.success(t('admin.aiPanel.messages.credentialSaved'));
    } catch (saveError: any) {
      toast.error(credentialSaveErrorMessage(saveError));
    } finally {
      setCredentialSaving(false);
    }
  };

  const savePreset = async () => {
    if (!presetName.trim() || !presetModelId) {
      return;
    }
    setPresetSaving(true);
    try {
      if (editingPreset) {
        await adminApi.updateModelPreset(editingPreset.id, {
          presetName: presetName.trim(),
          systemPrompt: presetSystemPrompt.trim() || null,
          temperature: parseNumber(presetTemperature),
          topP: parseNumber(presetTopP),
          maxOutputTokensOverride: parseInteger(presetMaxTokens),
          extraParametersJson: editingPreset.extraParams ?? {},
        });
      } else {
        await adminApi.createModelPreset({
          ...scopeParams(selectedScope),
          modelCatalogId: presetModelId,
          presetName: presetName.trim(),
          systemPrompt: presetSystemPrompt.trim() || null,
          temperature: parseNumber(presetTemperature),
          topP: parseNumber(presetTopP),
          maxOutputTokensOverride: parseInteger(presetMaxTokens),
          extraParametersJson: {},
        });
      }
      resetPresetDialog();
      setReloadKey(value => value + 1);
      toast.success(t('admin.aiPanel.messages.presetSaved'));
    } catch (_saveError: any) {
      toast.error(t('admin.aiPanel.messages.presetSaveFailed'));
    } finally {
      setPresetSaving(false);
    }
  };

  const saveBinding = async (purpose: AIPurpose) => {
    const resolved = resolveBinding(purpose);
    if (!bindingCredentialId || !bindingPresetId) {
      return;
    }
    setBindingSaving(true);
    try {
      if (resolved.localBinding) {
        await adminApi.updateBinding(resolved.localBinding.id, {
          providerCredentialId: bindingCredentialId,
          modelPresetId: bindingPresetId,
          bindingState: 'active',
        });
      } else {
        await adminApi.createBinding({
          ...scopeParams(selectedScope),
          bindingPurpose: purpose,
          providerCredentialId: bindingCredentialId,
          modelPresetId: bindingPresetId,
        });
      }
      setEditingPurpose(null);
      setBindingCredentialId('');
      setBindingPresetId('');
      setReloadKey(value => value + 1);
      toast.success(t('admin.aiPanel.messages.bindingSaved'));
    } catch (_saveError: any) {
      toast.error(t('admin.aiPanel.messages.bindingSaveFailed'));
    } finally {
      setBindingSaving(false);
    }
  };

  const resetBinding = async (purpose: AIPurpose) => {
    const resolved = resolveBinding(purpose);
    if (!resolved.localBinding || selectedScope === 'instance') {
      return;
    }
    try {
      await adminApi.deleteBinding(resolved.localBinding.id);
      setEditingPurpose(null);
      setReloadKey(value => value + 1);
      toast.success(t('admin.aiPanel.messages.overrideRemoved'));
    } catch (_deleteError: any) {
      toast.error(t('admin.aiPanel.messages.overrideRemoveFailed'));
    }
  };

  return (
    <div className="space-y-6">
      <div className="workbench-surface p-5">
        <div className="flex flex-col gap-4 xl:flex-row xl:items-start xl:justify-between">
          <div className="max-w-2xl">
            <h2 className="text-base font-bold tracking-tight">{t('admin.aiPanel.title')}</h2>
            <p className="mt-1 text-sm text-muted-foreground">
              {t('admin.aiPanel.description')}
            </p>
          </div>
          <div className="grid gap-2 sm:grid-cols-3 xl:min-w-[520px]">
            {[
              {
                kind: 'instance' as const,
                title: t('admin.aiPanel.scopeCards.instanceTitle'),
                detail: t('admin.aiPanel.scopeCards.instanceDetail'),
                disabled: false,
              },
              {
                kind: 'workspace' as const,
                title: activeWorkspace ? activeWorkspace.name : t('admin.aiPanel.scopeCards.workspaceTitle'),
                detail: activeWorkspace
                  ? t('admin.aiPanel.scopeCards.workspaceDetail')
                  : t('admin.aiPanel.scopeCards.workspaceMissingDetail'),
                disabled: !activeWorkspace,
              },
              {
                kind: 'library' as const,
                title: activeLibrary ? activeLibrary.name : t('admin.aiPanel.scopeCards.libraryTitle'),
                detail: activeLibrary
                  ? t('admin.aiPanel.scopeCards.libraryDetail')
                  : t('admin.aiPanel.scopeCards.libraryMissingDetail'),
                disabled: !activeLibrary,
              },
            ].map(scope => (
              <button
                key={scope.kind}
                type="button"
                disabled={scope.disabled}
                onClick={() => setSelectedScope(scope.kind)}
                className={`rounded-2xl border p-4 text-left transition ${selectedScope === scope.kind ? 'border-primary bg-primary/5 shadow-lifted' : 'border-border bg-card hover:border-primary/40'} ${scope.disabled ? 'cursor-not-allowed opacity-50' : ''}`}
              >
                <div className="text-sm font-semibold">{scope.title}</div>
                <p className="mt-1 text-xs text-muted-foreground">{scope.detail}</p>
              </button>
            ))}
          </div>
        </div>
      </div>

      {showMissingInstanceNotice && (
        <div className="rounded-2xl border border-status-warning/20 bg-status-warning/5 p-4 text-sm text-status-warning">
          {t('admin.aiPanel.notices.missingInstanceBaseline')}
        </div>
      )}

      {loading ? (
        <div className="grid gap-6 xl:grid-cols-[minmax(0,1.75fr)_360px]">
          <div className="space-y-4">
            {PURPOSE_ORDER.map(purpose => (
              <div key={purpose} className="workbench-surface p-5">
                <div className="animate-pulse space-y-4">
                  <div className="flex items-center justify-between gap-4">
                    <div className="space-y-2">
                      <div className="h-4 w-36 rounded-full bg-muted/70" />
                      <div className="h-3 w-56 rounded-full bg-muted/50" />
                    </div>
                    <div className="h-9 w-28 rounded-xl bg-muted/60" />
                  </div>
                  <div className="rounded-2xl border border-dashed border-border/70 bg-surface-sunken p-4 text-sm text-muted-foreground">
                    <div className="flex items-center gap-2">
                      <Loader2 className="h-4 w-4 animate-spin" />
                      {t('admin.aiPanel.loadingPurpose', { purpose: purposeLabel(purpose) })}
                    </div>
                  </div>
                </div>
              </div>
            ))}
          </div>

          <div className="space-y-4">
            {[t('admin.aiPanel.credentialsTitle'), t('admin.aiPanel.presetsTitle')].map(title => (
              <div key={title} className="workbench-surface p-5">
                <div className="animate-pulse space-y-4">
                  <div className="flex items-start justify-between gap-3">
                    <div className="space-y-2">
                      <div className="h-4 w-40 rounded-full bg-muted/70" />
                      <div className="h-3 w-44 rounded-full bg-muted/50" />
                    </div>
                    <div className="h-9 w-20 rounded-xl bg-muted/60" />
                  </div>
                  <div className="space-y-3">
                    <div className="rounded-xl border border-border/60 p-4">
                      <div className="h-4 w-32 rounded-full bg-muted/60" />
                      <div className="mt-2 h-3 w-48 rounded-full bg-muted/45" />
                    </div>
                    <div className="rounded-xl border border-border/60 p-4">
                      <div className="h-4 w-28 rounded-full bg-muted/60" />
                      <div className="mt-2 h-3 w-40 rounded-full bg-muted/45" />
                    </div>
                  </div>
                </div>
              </div>
            ))}
          </div>
        </div>
      ) : error ? (
        <div className="rounded-2xl border border-status-failed/20 bg-status-failed/5 p-5 text-sm text-status-failed">
          {error}
        </div>
      ) : (
        <div className="grid gap-6 xl:grid-cols-[minmax(0,1.75fr)_360px]">
          <div className="space-y-4">
            {PURPOSE_ORDER.map(purpose => {
              const resolved = resolveBinding(purpose);
              const credential = availableCredentials.find(entry => entry.id === resolved.effectiveBinding?.credentialId);
              const preset = availablePresets.find(entry => entry.id === resolved.effectiveBinding?.presetId);
              const presetModel = preset ? modelById.get(preset.modelCatalogId) : undefined;
              const bindingModelUnavailable =
                credential && preset
                  ? !isModelAvailableForCredential(presetModel, credential, modelsByCredentialId)
                  : false;
              const selectedCredential = availableCredentials.find(entry => entry.id === bindingCredentialId);
              const selectedCredentialModelSet = selectedCredential?.providerKind === 'ollama'
                ? new Set((modelsByCredentialId[selectedCredential.id] ?? []).map(entry => entry.id))
                : null;
              const presetOptions = availablePresets
                .filter(entry => entry.allowedBindingPurposes.includes(purpose))
                .filter(entry => !selectedCredential || entry.providerId === selectedCredential.providerId);
              const selectedPresetUnavailable = selectedCredential?.providerKind === 'ollama'
                && bindingPresetId !== ''
                && selectedCredentialModelSet !== null
                && !selectedCredentialModelSet.has(
                  availablePresets.find(entry => entry.id === bindingPresetId)?.modelCatalogId ?? '',
                );
              return (
                <div key={purpose} className="workbench-surface p-5">
                  <div className="flex flex-col gap-4 lg:flex-row lg:items-start lg:justify-between">
                    <div className="min-w-0">
                      <div className="flex flex-wrap items-center gap-2">
                        <h3 className="text-sm font-bold tracking-tight">{purposeLabel(purpose)}</h3>
                        {resolved.sourceKind && (
                          <span className={`status-badge ${badgeClass(resolved.sourceKind === 'instance' ? 'ready' : resolved.sourceKind === 'workspace' ? 'warning' : 'failed')}`}>
                            {scopeLabel(resolved.sourceKind)}
                          </span>
                        )}
                      </div>
                      {resolved.effectiveBinding && credential && preset ? (
                        <div className="mt-2 space-y-1 text-sm">
                          <div>
                            <span className="text-muted-foreground">{t('admin.aiPanel.fields.credential')}:</span>{' '}
                            <span className="font-semibold">{credential.label}</span>
                            <span className="text-muted-foreground"> · {credential.providerName}</span>
                          </div>
                          <div>
                            <span className="text-muted-foreground">{t('admin.aiPanel.fields.preset')}:</span>{' '}
                            <span className="font-semibold">{formatPresetLabel(preset)}</span>
                          </div>
                          {bindingModelUnavailable && (
                            <div className="flex items-center gap-2 text-status-warning">
                              <AlertTriangle className="h-4 w-4" />
                              {t('admin.aiPanel.messages.bindingModelUnavailable', { model: preset.modelName })}
                            </div>
                          )}
                        </div>
                      ) : (
                        <div className="mt-3 flex items-center gap-2 text-sm text-status-warning">
                          <AlertTriangle className="h-4 w-4" />
                          {t('admin.aiPanel.empty.noEffectiveBinding')}
                        </div>
                      )}
                    </div>
                    <div className="flex shrink-0 flex-wrap gap-2">
                      <Button size="sm" variant="outline" onClick={() => openBindingEditor(purpose)}>
                        <Settings2 className="mr-1.5 h-3.5 w-3.5" />
                        {resolved.localBinding
                          ? t('admin.aiPanel.actions.editHere')
                          : selectedScope === 'instance'
                            ? t('admin.aiPanel.actions.setDefault')
                            : t('admin.aiPanel.actions.createOverride')}
                      </Button>
                      {resolved.localBinding && selectedScope !== 'instance' && (
                        <Button size="sm" variant="outline" onClick={() => void resetBinding(purpose)}>
                          {t('admin.aiPanel.actions.resetToInherited')}
                        </Button>
                      )}
                    </div>
                  </div>

                  {editingPurpose === purpose && (
                    <div className="mt-4 rounded-2xl border border-border/60 bg-surface-sunken p-4">
                      <div className="grid gap-4 lg:grid-cols-2">
                        <div>
                          <Label className="text-xs font-semibold">{t('admin.aiPanel.fields.credential')}</Label>
                          <Select value={bindingCredentialId} onValueChange={value => {
                            setBindingCredentialId(value);
                            setBindingPresetId('');
                          }}>
                            <SelectTrigger className="mt-2 h-10 text-sm">
                              <SelectValue placeholder={t('admin.aiPanel.placeholders.selectCredential')} />
                            </SelectTrigger>
                            <SelectContent>
                              {availableCredentials.map(entry => (
                                <SelectItem key={entry.id} value={entry.id}>
                                  {entry.label} · {scopeLabel(entry.scopeKind)}
                                </SelectItem>
                              ))}
                            </SelectContent>
                          </Select>
                        </div>
                        <div>
                          <Label className="text-xs font-semibold">{t('admin.aiPanel.fields.modelPreset')}</Label>
                          <Select value={bindingPresetId} onValueChange={setBindingPresetId}>
                            <SelectTrigger className="mt-2 h-10 text-sm">
                              <SelectValue placeholder={t('admin.aiPanel.placeholders.selectPreset')} />
                            </SelectTrigger>
                            <SelectContent>
                              {presetOptions.map(entry => (
                                <SelectItem key={entry.id} value={entry.id}>
                                  {selectedCredentialModelSet !== null
                                    && !selectedCredentialModelSet.has(entry.modelCatalogId)
                                    ? `${formatPresetLabel(entry)} · ${t('admin.aiPanel.unavailableBadge')}`
                                    : formatPresetLabel(entry)}
                                </SelectItem>
                              ))}
                            </SelectContent>
                          </Select>
                        </div>
                      </div>
                      {selectedPresetUnavailable && (
                        <div className="mt-3 flex items-center gap-2 text-sm text-status-warning">
                          <AlertTriangle className="h-4 w-4" />
                          {t('admin.aiPanel.messages.selectedPresetUnavailable')}
                        </div>
                      )}
                      <div className="mt-4 flex gap-2">
                        <Button size="sm" disabled={!bindingCredentialId || !bindingPresetId || bindingSaving || Boolean(selectedPresetUnavailable)} onClick={() => void saveBinding(purpose)}>
                          {bindingSaving ? <><Loader2 className="mr-1.5 h-3.5 w-3.5 animate-spin" /> {t('admin.saving')}</> : t('admin.save')}
                        </Button>
                        <Button size="sm" variant="outline" onClick={() => setEditingPurpose(null)}>
                          {t('admin.cancel')}
                        </Button>
                      </div>
                    </div>
                  )}
                </div>
              );
            })}
          </div>

          <div className="space-y-4">
            <div className="workbench-surface p-5">
              <div className="flex items-start justify-between gap-3">
                <div>
                  <h3 className="text-sm font-bold tracking-tight">{t('admin.aiPanel.credentialsTitle')}</h3>
                  <p className="mt-1 text-xs text-muted-foreground">{t('admin.aiPanel.credentialsDescription')}</p>
                </div>
                <Button size="sm" variant="outline" onClick={() => setCredentialOpen(true)}>
                  <KeyRound className="mr-1.5 h-3.5 w-3.5" /> {t('admin.add')}
                </Button>
              </div>
              <div className="mt-4 space-y-3">
                {localCredentials.length === 0 ? (
                  <div className="rounded-xl border border-dashed p-4 text-sm text-muted-foreground">{t('admin.aiPanel.empty.noLocalCredentials')}</div>
                ) : localCredentials.map(entry => (
                  <div key={entry.id} className="rounded-xl border border-border/60 p-4">
                    <div className="text-sm font-semibold">{entry.label}</div>
                    <div className="mt-1 text-xs text-muted-foreground">
                      {entry.providerName}
                      {entry.baseUrl ? ` · ${baseUrlForProviderInput(entry.providerKind, entry.baseUrl)}` : ''}
                    </div>
                    <div className="mt-1 text-xs font-mono text-muted-foreground">{entry.apiKeySummary || t('admin.aiPanel.tokenOptional')}</div>
                    <div className="mt-3 flex items-center justify-between">
                      <span className={`status-badge ${badgeClass(entry.state === 'active' ? 'ready' : entry.state === 'invalid' ? 'failed' : 'warning')}`}>{credentialStateLabel(entry.state)}</span>
                      <Button size="sm" variant="outline" onClick={() => {
                        const provider = providers.find(providerEntry => providerEntry.id === entry.providerId);
                        setEditingCredential(entry);
                        setCredentialProviderId(entry.providerId);
                        setCredentialLabel(entry.label);
                        setCredentialBaseUrl(baseUrlForProviderInput(entry.providerKind, entry.baseUrl ?? provider?.defaultBaseUrl));
                        setCredentialApiKey('');
                        setCredentialOpen(true);
                      }}>
                        {t('admin.edit')}
                      </Button>
                    </div>
                  </div>
                ))}
              </div>
            </div>

            <div className="workbench-surface p-5">
              <div className="flex items-start justify-between gap-3">
                <div>
                  <h3 className="text-sm font-bold tracking-tight">{t('admin.aiPanel.presetsTitle')}</h3>
                  <p className="mt-1 text-xs text-muted-foreground">{t('admin.aiPanel.presetsDescription')}</p>
                </div>
                <Button size="sm" variant="outline" onClick={() => setPresetOpen(true)}>
                  <Brain className="mr-1.5 h-3.5 w-3.5" /> {t('admin.add')}
                </Button>
              </div>
              <div className="mt-4 space-y-3">
                {localPresets.length === 0 ? (
                  <div className="rounded-xl border border-dashed p-4 text-sm text-muted-foreground">{t('admin.aiPanel.empty.noLocalPresets')}</div>
                ) : localPresets.map(entry => (
                  <div key={entry.id} className="rounded-xl border border-border/60 p-4">
                    {(() => {
                      const model = modelById.get(entry.modelCatalogId);
                      const presetModelUnavailable =
                        entry.providerKind === 'ollama' && model?.availabilityState === 'unavailable';
                      return (
                        <>
                    <div className="text-sm font-semibold">{entry.presetName}</div>
                    <div className="mt-1 text-xs text-muted-foreground">{entry.providerName} · {entry.modelName}</div>
                    <div className="mt-2 text-xs text-muted-foreground">
                      {t('admin.aiPanel.purposeSummary', { purposes: formatPurposeList(entry.allowedBindingPurposes) })}
                    </div>
                    {presetModelUnavailable && (
                      <div className="mt-3 flex items-center gap-2 text-sm text-status-warning">
                        <AlertTriangle className="h-4 w-4" />
                        {t('admin.aiPanel.messages.presetModelUnavailable', { model: entry.modelName })}
                      </div>
                    )}
                    <div className="mt-3 flex justify-end">
                      <Button size="sm" variant="outline" onClick={() => {
                        setEditingPreset(entry);
                        setPresetName(entry.presetName);
                        setPresetModelId(entry.modelCatalogId);
                        setPresetSystemPrompt(entry.systemPrompt ?? '');
                        setPresetTemperature(entry.temperature !== undefined ? String(entry.temperature) : '');
                        setPresetTopP(entry.topP !== undefined ? String(entry.topP) : '');
                        setPresetMaxTokens(entry.maxOutputTokens !== undefined ? String(entry.maxOutputTokens) : '');
                        setPresetOpen(true);
                      }}>
                        {t('admin.edit')}
                      </Button>
                    </div>
                        </>
                      );
                    })()}
                  </div>
                ))}
              </div>
            </div>
          </div>
        </div>
      )}

      <Dialog open={credentialOpen} onOpenChange={open => { if (!open) { resetCredentialDialog(); } else { setCredentialOpen(true); } }}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{editingCredential ? t('admin.aiPanel.dialogs.editCredentialTitle') : t('admin.aiPanel.dialogs.addCredentialTitle')}</DialogTitle>
            <DialogDescription>{t('admin.aiPanel.dialogs.credentialDescription')}</DialogDescription>
          </DialogHeader>
          <div className="space-y-4">
            <div>
              <Label>{t('admin.provider')}</Label>
              <Select value={credentialProviderId} onValueChange={setCredentialProviderId} disabled={Boolean(editingCredential)}>
                <SelectTrigger className="mt-2 h-10">
                  <SelectValue placeholder={t('admin.aiPanel.placeholders.selectProvider')} />
                </SelectTrigger>
                <SelectContent>
                  {providers.map(entry => (
                    <SelectItem key={entry.id} value={entry.id}>{entry.displayName}</SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
            <div>
              <Label>{t('admin.label')}</Label>
              <Input className="mt-2" value={credentialLabel} onChange={event => setCredentialLabel(event.target.value)} placeholder={t('admin.aiPanel.placeholders.credentialLabel')} />
            </div>
            {(selectedProvider?.baseUrlRequired || selectedProvider?.defaultBaseUrl) && (
              <div>
                <Label>{selectedProvider?.baseUrlRequired ? t('admin.aiPanel.fields.baseUrl') : t('admin.aiPanel.fields.baseUrlOptional')}</Label>
                <Input className="mt-2 font-mono text-xs" value={credentialBaseUrl} onChange={event => setCredentialBaseUrl(event.target.value)} placeholder={baseUrlForProviderInput(selectedProvider.kind, selectedProvider.defaultBaseUrl)} />
                {selectedProvider?.kind === 'ollama' && (
                  <p className="mt-2 text-xs text-muted-foreground">{t('login.ollamaAddressHint')}</p>
                )}
              </div>
            )}
            <div>
              <Label>{selectedProvider?.apiKeyRequired ? t('admin.apiKey') : t('admin.aiPanel.fields.tokenOptional')}</Label>
              <Input type="password" className="mt-2" value={credentialApiKey} onChange={event => setCredentialApiKey(event.target.value)} placeholder={editingCredential ? t('admin.aiPanel.placeholders.keepSecret') : t('admin.aiPanel.placeholders.apiKey')} />
            </div>
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={resetCredentialDialog}>{t('admin.cancel')}</Button>
            <Button disabled={!credentialProviderId || !credentialLabel.trim() || Boolean(selectedProvider?.baseUrlRequired && !credentialBaseUrl.trim()) || Boolean(!editingCredential && selectedProvider?.apiKeyRequired && !credentialApiKey.trim()) || credentialSaving} onClick={() => void saveCredential()}>
              {credentialSaving ? <><Loader2 className="mr-1.5 h-3.5 w-3.5 animate-spin" /> {t('admin.saving')}</> : t('admin.save')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog open={presetOpen} onOpenChange={open => { if (!open) { resetPresetDialog(); } else { setPresetOpen(true); } }}>
        <DialogContent className="max-w-lg">
          <DialogHeader>
            <DialogTitle>{editingPreset ? t('admin.aiPanel.dialogs.editPresetTitle') : t('admin.aiPanel.dialogs.addPresetTitle')}</DialogTitle>
            <DialogDescription>{t('admin.aiPanel.dialogs.presetDescription')}</DialogDescription>
          </DialogHeader>
          <div className="space-y-4">
            <div>
              <Label>{t('admin.presetName')}</Label>
              <Input className="mt-2" value={presetName} onChange={event => setPresetName(event.target.value)} placeholder={t('admin.aiPanel.placeholders.presetName')} />
            </div>
            <div>
              <Label>{t('admin.model')}</Label>
              <Select value={presetModelId} onValueChange={setPresetModelId} disabled={Boolean(editingPreset)}>
                <SelectTrigger className="mt-2 h-10">
                  <SelectValue placeholder={t('admin.aiPanel.placeholders.selectModel')} />
                </SelectTrigger>
                <SelectContent>
                  {selectableModels.map(entry => (
                    <SelectItem key={entry.id} value={entry.id}>{formatModelLabel(entry, providers)}</SelectItem>
                  ))}
                </SelectContent>
              </Select>
              {selectableModels.length === 0 && (
                <p className="mt-2 text-xs text-status-warning">{t('admin.aiPanel.empty.noSelectableModels')}</p>
              )}
            </div>
            <div>
              <Label>{t('admin.systemPrompt')}</Label>
              <Textarea className="mt-2 min-h-[120px]" value={presetSystemPrompt} onChange={event => setPresetSystemPrompt(event.target.value)} placeholder={t('admin.aiPanel.placeholders.systemPrompt')} />
            </div>
            <div className="grid gap-4 sm:grid-cols-2">
              <div>
                <Label>{t('admin.temperature')}</Label>
                <Input className="mt-2" value={presetTemperature} onChange={event => setPresetTemperature(event.target.value)} placeholder={t('admin.aiPanel.placeholders.defaultValue')} />
              </div>
              <div>
                <Label>{t('admin.topP')}</Label>
                <Input className="mt-2" value={presetTopP} onChange={event => setPresetTopP(event.target.value)} placeholder={t('admin.aiPanel.placeholders.defaultValue')} />
              </div>
            </div>
            <div>
              <Label>{t('admin.maxOutputTokens')}</Label>
              <Input className="mt-2" value={presetMaxTokens} onChange={event => setPresetMaxTokens(event.target.value)} placeholder={t('admin.aiPanel.placeholders.defaultValue')} />
            </div>
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={resetPresetDialog}>{t('admin.cancel')}</Button>
            <Button disabled={!presetName.trim() || !presetModelId || presetSaving} onClick={() => void savePreset()}>
              {presetSaving ? <><Loader2 className="mr-1.5 h-3.5 w-3.5 animate-spin" /> {t('admin.saving')}</> : t('admin.save')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
