import { useCallback, useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { toast } from 'sonner';
import { AlertTriangle, Brain, KeyRound, Loader2, Search, Settings2 } from 'lucide-react';

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
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { ScrollArea } from '@/components/ui/scroll-area';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select';
import { Textarea } from '@/components/ui/textarea';

import {
  mapBindingList,
  mapCredentialList,
  mapModelList,
  mapPresetList,
  mapProviderList,
} from '@/adapters/ai';

type BindingResolution = {
  localBinding: AIBindingAssignment | null;
  effectiveBinding: AIBindingAssignment | null;
  sourceKind: AIScopeKind | null;
};

type CredentialModelLoadState = 'loading' | 'ready' | 'failed';

const PURPOSE_ORDER: AIPurpose[] = [
  'extract_graph',
  'embed_chunk',
  'query_compile',
  'query_answer',
  'vision',
];

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
  if (!model || !credential) {
    return true;
  }
  const discoveredModels = modelsByCredentialId[credential.id];
  if (!discoveredModels) {
    if (model.availableCredentialIds.includes(credential.id)) {
      return true;
    }
    return model.availabilityState !== 'unavailable';
  }
  return discoveredModels.some(entry => entry.id === model.id);
}

function formatModelLabel(model: AIModelOption, providers: AIProvider[]) {
  const provider = providers.find(entry => entry.id === model.providerCatalogId);
  return provider ? `${provider.displayName} · ${model.modelName}` : model.modelName;
}

function matchesFilter(values: Array<string | undefined>, filter: string) {
  const normalized = filter.trim().toLocaleLowerCase();
  if (!normalized) {
    return true;
  }
  return values.some(value => value?.toLocaleLowerCase().includes(normalized));
}

function compareByUpdatedAtDesc(left: { updatedAt: string; id: string }, right: { updatedAt: string; id: string }) {
  return right.updatedAt.localeCompare(left.updatedAt) || left.id.localeCompare(right.id);
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
  const [credentialModelLoadState, setCredentialModelLoadState] = useState<Record<string, CredentialModelLoadState>>({});
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
  const [credentialSearch, setCredentialSearch] = useState('');
  const [presetSearch, setPresetSearch] = useState('');

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
      return {};
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
        const localScopeParams = scopeParams(selectedScope);
        const currentVisibleParams = visibleParams(selectedScope);
        const localCredentialRequest = adminApi.listCredentials(localScopeParams);
        const localPresetRequest = adminApi.listModelPresets(localScopeParams);
        const visibleCredentialRequest =
          selectedScope === 'instance' ? localCredentialRequest : adminApi.listCredentials(currentVisibleParams);
        const visiblePresetRequest =
          selectedScope === 'instance' ? localPresetRequest : adminApi.listModelPresets(currentVisibleParams);
        const [
          providerRaw,
          modelRaw,
          localCredentialRaw,
          localPresetRaw,
          visibleCredentialRaw,
          visiblePresetRaw,
          instanceBindingRaw,
          workspaceBindingRaw,
          libraryBindingRaw,
        ] = await Promise.all([
          adminApi.listProviders(),
          adminApi.listModels(currentVisibleParams),
          localCredentialRequest,
          localPresetRequest,
          visibleCredentialRequest,
          visiblePresetRequest,
          adminApi.listBindings({ scopeKind: 'instance' }),
          activeWorkspace?.id ? adminApi.listBindings({ scopeKind: 'workspace', workspaceId: activeWorkspace.id }) : Promise.resolve([]),
          activeLibrary?.id ? adminApi.listBindings({ scopeKind: 'library', workspaceId: activeWorkspace?.id, libraryId: activeLibrary.id }) : Promise.resolve([]),
        ]);
        if (cancelled) {
          return;
        }
        const providerList = mapProviderList(providerRaw);
        const modelList = mapModelList(modelRaw);
        const localCredentialList = mapCredentialList(localCredentialRaw, providerList);
        const visibleCredentialList = mapCredentialList(visibleCredentialRaw, providerList);
        setProviders(providerList);
        setModels(modelList);
        setModelsByCredentialId({});
        setCredentialModelLoadState({});
        setLocalCredentials(localCredentialList);
        setAvailableCredentials(visibleCredentialList);
        setLocalPresets(mapPresetList(localPresetRaw, providerList, modelList));
        setAvailablePresets(mapPresetList(visiblePresetRaw, providerList, modelList));
        setInstanceBindings(mapBindingList(instanceBindingRaw));
        setWorkspaceBindings(mapBindingList(workspaceBindingRaw));
        setLibraryBindings(mapBindingList(libraryBindingRaw));
      } catch (_loadError: unknown) {
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

  useEffect(() => {
    if (!editingPurpose || !bindingCredentialId) {
      return;
    }
    const credential = availableCredentials.find(entry => entry.id === bindingCredentialId);
    if (!credential || credential.providerKind !== 'ollama') {
      return;
    }
    if (modelsByCredentialId[credential.id] || credentialModelLoadState[credential.id] === 'loading' || credentialModelLoadState[credential.id] === 'failed') {
      return;
    }

    let cancelled = false;
    setCredentialModelLoadState(current => ({ ...current, [credential.id]: 'loading' }));
    const currentVisibleParams = visibleParams(selectedScope);
    void adminApi.listModels({
      providerCatalogId: credential.providerId,
      credentialId: credential.id,
      ...currentVisibleParams,
    }).then(raw => {
      if (cancelled) {
        return;
      }
      setModelsByCredentialId(current => ({ ...current, [credential.id]: mapModelList(raw) }));
      setCredentialModelLoadState(current => ({ ...current, [credential.id]: 'ready' }));
    }).catch(() => {
      if (cancelled) {
        return;
      }
      setCredentialModelLoadState(current => ({ ...current, [credential.id]: 'failed' }));
      toast.error(t('admin.aiPanel.messages.credentialModelRefreshFailed'));
    });

    return () => {
      cancelled = true;
    };
  }, [availableCredentials, bindingCredentialId, credentialModelLoadState, editingPurpose, modelsByCredentialId, selectedScope, t, visibleParams]);

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
    return entry.availabilityState !== 'unavailable';
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
    } catch (saveError: unknown) {
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
    } catch (_saveError: unknown) {
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
    } catch (_saveError: unknown) {
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
    } catch (_deleteError: unknown) {
      toast.error(t('admin.aiPanel.messages.overrideRemoveFailed'));
    }
  };

  // `localBindingCount` drives the "X/Y configured" badge next to the
  // bindings header. Provider inventory + per-scope binding metrics
  // were removed when the page was simplified — the same numbers are
  // already visible in the binding cards and the per-list count
  // badges below, so showing them up top was redundant noise.
  const localBindingCount = bindingsForScope.length;
  const filteredLocalCredentials = localCredentials
    .filter(entry => matchesFilter([
      entry.label,
      entry.providerName,
      entry.providerKind,
      entry.baseUrl,
      credentialStateLabel(entry.state),
    ], credentialSearch))
    .slice()
    .sort(compareByUpdatedAtDesc);
  const filteredLocalPresets = localPresets
    .filter(entry => matchesFilter([
      entry.presetName,
      entry.providerName,
      entry.providerKind,
      entry.modelName,
      formatPurposeList(entry.allowedBindingPurposes),
    ], presetSearch))
    .slice()
    .sort(compareByUpdatedAtDesc);

  // Scope tab metadata kept inline so the disabled/title computation
  // happens once per render and the JSX stays readable.
  const scopeTabs: Array<{ kind: AIScopeKind; title: string; disabled: boolean }> = [
    {
      kind: 'instance',
      title: t('admin.aiPanel.scopeCards.instanceTitle'),
      disabled: false,
    },
    {
      kind: 'workspace',
      title: activeWorkspace?.name ?? t('admin.aiPanel.scopeCards.workspaceTitle'),
      disabled: !activeWorkspace,
    },
    {
      kind: 'library',
      title: activeLibrary?.name ?? t('admin.aiPanel.scopeCards.libraryTitle'),
      disabled: !activeLibrary,
    },
  ];

  return (
    <div className="space-y-5">
      {/* ── Header: slim title row + a real Radix-style segmented scope
           tab strip below it. The previous design tried to balance the
           scope picker on the right of the title and it kept wrapping
           to a second line on mid-width viewports. A dedicated full-
           width strip aligns with the existing admin tab pattern and
           reads like a single canonical "what am I editing now"
           anchor. */}
      <div>
        <h2 className="text-base font-bold tracking-tight">{t('admin.aiPanel.title')}</h2>
        <p className="mt-1 text-xs text-muted-foreground">{t('admin.aiPanel.description')}</p>
      </div>

      <div className="flex flex-wrap items-center gap-1 rounded-2xl border border-border/70 bg-surface-sunken p-1 shadow-sm">
        {scopeTabs.map(scope => (
          <button
            key={scope.kind}
            type="button"
            disabled={scope.disabled}
            onClick={() => setSelectedScope(scope.kind)}
            className={`flex-1 rounded-xl px-4 py-2 text-sm font-semibold transition ${
              selectedScope === scope.kind
                ? 'bg-primary text-primary-foreground shadow-sm'
                : 'text-muted-foreground hover:bg-muted/60'
            } ${scope.disabled ? 'cursor-not-allowed opacity-40' : ''}`}
            title={scope.title}
          >
            {scope.title}
          </button>
        ))}
      </div>

      {showMissingInstanceNotice && (
        <div className="rounded-2xl border border-status-warning/20 bg-status-warning/5 p-3 text-sm text-status-warning">
          {t('admin.aiPanel.notices.missingInstanceBaseline')}
        </div>
      )}

      {loading ? (
        <div className="space-y-6">
          <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-4">
            {Array.from({ length: 4 }).map((_, index) => (
              <div key={index} className="workbench-surface p-5 animate-pulse">
                <div className="h-8 w-16 rounded-full bg-muted/60" />
                <div className="mt-3 h-3 w-28 rounded-full bg-muted/45" />
              </div>
            ))}
          </div>
          <div className="grid gap-4 md:grid-cols-2">
            {PURPOSE_ORDER.map(purpose => (
              <div key={purpose} className="workbench-surface p-5 animate-pulse space-y-4">
                <div className="h-4 w-40 rounded-full bg-muted/60" />
                <div className="h-3 w-48 rounded-full bg-muted/45" />
                <div className="h-24 rounded-3xl bg-muted/40" />
              </div>
            ))}
          </div>
          <div className="grid gap-6 xl:grid-cols-2">
            {Array.from({ length: 2 }).map((_, index) => (
              <div key={index} className="workbench-surface p-5 animate-pulse space-y-4">
                <div className="h-4 w-40 rounded-full bg-muted/60" />
                <div className="h-10 rounded-2xl bg-muted/45" />
                <div className="space-y-3">
                  <div className="h-24 rounded-3xl bg-muted/35" />
                  <div className="h-24 rounded-3xl bg-muted/35" />
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
        <>
          <div className="space-y-3">
            <div className="flex items-center gap-2">
              <h3 className="text-sm font-bold tracking-tight">{t('admin.aiPanel.sections.bindingsTitle')}</h3>
              <Badge variant="outline">{localBindingCount}/{PURPOSE_ORDER.length}</Badge>
            </div>

            <div className="grid gap-4 md:grid-cols-2">
              {PURPOSE_ORDER.map(purpose => {
                const resolved = resolveBinding(purpose);
                const credential = availableCredentials.find(entry => entry.id === resolved.effectiveBinding?.credentialId);
                const preset = availablePresets.find(entry => entry.id === resolved.effectiveBinding?.presetId);
                const presetModel = preset ? modelById.get(preset.modelCatalogId) : undefined;
                const bindingModelUnavailable =
                  credential && preset
                    ? !isModelAvailableForCredential(presetModel, credential, modelsByCredentialId)
                    : false;
                const selectedCredential = availableCredentials.find(entry => entry.id === bindingCredentialId) ?? null;
                const selectedCredentialModels = selectedCredential ? modelsByCredentialId[selectedCredential.id] : undefined;
                const selectedCredentialModelSet = selectedCredentialModels
                  ? new Set(selectedCredentialModels.map(entry => entry.id))
                  : null;
                const selectedCredentialLoadState = selectedCredential ? credentialModelLoadState[selectedCredential.id] : undefined;
                const presetOptions = availablePresets
                  .filter(entry => entry.allowedBindingPurposes.includes(purpose))
                  .filter(entry => !selectedCredential || entry.providerId === selectedCredential.providerId);
                const selectedPreset = availablePresets.find(entry => entry.id === bindingPresetId);
                const selectedPresetUnavailable =
                  selectedCredential !== null
                  && bindingPresetId !== ''
                  && !isModelAvailableForCredential(
                    modelById.get(selectedPreset?.modelCatalogId ?? ''),
                    selectedCredential,
                    modelsByCredentialId,
                  );
                // Source-scope badge only matters when the active
                // binding lives at a DIFFERENT scope than the one the
                // operator is currently editing — otherwise it just
                // restates "this scope's local value", which is the
                // default and never news.
                const showSourceBadge =
                  resolved.sourceKind != null && resolved.sourceKind !== selectedScope;
                return (
                  <div key={purpose} className="workbench-surface overflow-hidden p-4">
                    <div className="flex h-full flex-col gap-3">
                      <div className="flex items-start justify-between gap-3">
                        <div className="min-w-0">
                          <div className="flex flex-wrap items-center gap-2">
                            <h4 className="text-sm font-semibold tracking-tight">{purposeLabel(purpose)}</h4>
                            {showSourceBadge && resolved.sourceKind && (
                              <span className={`status-badge text-[10px] ${badgeClass(resolved.sourceKind === 'instance' ? 'ready' : resolved.sourceKind === 'workspace' ? 'warning' : 'failed')}`}>
                                {t('admin.aiPanel.labels.inheritedFrom', {
                                  scope: scopeLabel(resolved.sourceKind),
                                })}
                              </span>
                            )}
                          </div>
                        </div>
                        <Button size="sm" variant="outline" onClick={() => openBindingEditor(purpose)}>
                          <Settings2 className="mr-1.5 h-3.5 w-3.5" />
                          {resolved.localBinding
                            ? t('admin.aiPanel.actions.editHere')
                            : selectedScope === 'instance'
                              ? t('admin.aiPanel.actions.setDefault')
                              : t('admin.aiPanel.actions.createOverride')}
                        </Button>
                      </div>

                      {resolved.effectiveBinding && credential && preset ? (
                        // Compact single-row summary: "credential · preset · model"
                        // replaces the previous two stacked cards. Each binding
                        // already shows the provider name in the credential field,
                        // so the previous duplicated provider line under the preset
                        // was pure noise.
                        <div className="text-sm space-y-0.5">
                          <div className="flex items-center gap-2">
                            <KeyRound className="h-3.5 w-3.5 text-muted-foreground shrink-0" />
                            <span className="font-semibold truncate">{credential.label}</span>
                            <span className="text-xs text-muted-foreground">· {credential.providerName}</span>
                          </div>
                          <div className="flex items-center gap-2">
                            <Brain className="h-3.5 w-3.5 text-muted-foreground shrink-0" />
                            <span className="font-semibold truncate">{formatPresetLabel(preset)}</span>
                            <span className="text-xs text-muted-foreground">· {preset.modelName}</span>
                          </div>
                        </div>
                      ) : (
                        <div className="rounded-2xl border border-dashed border-status-warning/30 bg-status-warning/5 p-3 text-sm text-status-warning">
                          <div className="flex items-center gap-2">
                            <AlertTriangle className="h-4 w-4" />
                            {t('admin.aiPanel.empty.noEffectiveBinding')}
                          </div>
                        </div>
                      )}

                      {bindingModelUnavailable && (
                        <div className="rounded-2xl border border-status-warning/25 bg-status-warning/5 p-3 text-sm text-status-warning">
                          <div className="flex items-center gap-2">
                            <AlertTriangle className="h-4 w-4" />
                            {t('admin.aiPanel.messages.bindingModelUnavailable', { model: preset?.modelName ?? '' })}
                          </div>
                        </div>
                      )}

                      {editingPurpose === purpose && (
                        <div className="rounded-3xl border border-border/70 bg-surface-sunken p-4">
                          <div className="grid gap-4 xl:grid-cols-2">
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
                                      {selectedCredentialModelSet !== null && !selectedCredentialModelSet.has(entry.modelCatalogId)
                                        ? `${formatPresetLabel(entry)} · ${t('admin.aiPanel.unavailableBadge')}`
                                        : formatPresetLabel(entry)}
                                    </SelectItem>
                                  ))}
                                </SelectContent>
                              </Select>
                            </div>
                          </div>

                          {selectedCredentialLoadState === 'loading' && (
                            <div className="mt-3 flex items-center gap-2 text-sm text-muted-foreground">
                              <Loader2 className="h-4 w-4 animate-spin" />
                              {t('admin.aiPanel.messages.checkingCredentialModels')}
                            </div>
                          )}

                          {selectedPresetUnavailable && (
                            <div className="mt-3 flex items-center gap-2 text-sm text-status-warning">
                              <AlertTriangle className="h-4 w-4" />
                              {t('admin.aiPanel.messages.selectedPresetUnavailable')}
                            </div>
                          )}

                          <div className="mt-4 flex flex-wrap gap-2">
                            <Button size="sm" disabled={!bindingCredentialId || !bindingPresetId || bindingSaving || Boolean(selectedPresetUnavailable)} onClick={() => void saveBinding(purpose)}>
                              {bindingSaving ? <><Loader2 className="mr-1.5 h-3.5 w-3.5 animate-spin" /> {t('admin.saving')}</> : t('admin.save')}
                            </Button>
                            <Button size="sm" variant="outline" onClick={() => setEditingPurpose(null)}>
                              {t('admin.cancel')}
                            </Button>
                            {resolved.localBinding && selectedScope !== 'instance' && (
                              <Button size="sm" variant="outline" onClick={() => void resetBinding(purpose)}>
                                {t('admin.aiPanel.actions.resetToInherited')}
                              </Button>
                            )}
                          </div>
                        </div>
                      )}
                    </div>
                  </div>
                );
              })}
            </div>
          </div>

          <div className="grid gap-6 xl:grid-cols-2">
            <div className="workbench-surface overflow-hidden p-5">
              <div className="flex flex-col gap-4 sm:flex-row sm:items-start sm:justify-between">
                <div>
                  <div className="flex items-center gap-2">
                    <h3 className="text-sm font-bold tracking-tight">{t('admin.aiPanel.credentialsTitle')}</h3>
                    <Badge variant="outline">{localCredentials.length}</Badge>
                  </div>
                  <p className="mt-1 text-sm text-muted-foreground">{t('admin.aiPanel.credentialsDescription')}</p>
                </div>
                <div className="flex w-full flex-col gap-2 sm:w-[260px]">
                  <div className="relative">
                    <Search className="pointer-events-none absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-muted-foreground" />
                    <Input className="pl-9" value={credentialSearch} onChange={event => setCredentialSearch(event.target.value)} placeholder={t('admin.aiPanel.filters.credentialsSearch')} />
                  </div>
                  <Button size="sm" variant="outline" onClick={() => setCredentialOpen(true)}>
                    <KeyRound className="mr-1.5 h-3.5 w-3.5" /> {t('admin.add')}
                  </Button>
                </div>
              </div>
              <ScrollArea className="mt-4 h-[420px] pr-4">
                <div className="space-y-3">
                  {localCredentials.length === 0 ? (
                    <div className="rounded-2xl border border-dashed border-border/70 p-4 text-sm text-muted-foreground">
                      {t('admin.aiPanel.empty.noLocalCredentials')}
                    </div>
                  ) : filteredLocalCredentials.length === 0 ? (
                    <div className="rounded-2xl border border-dashed border-border/70 p-4 text-sm text-muted-foreground">
                      {t('admin.aiPanel.empty.noMatchingCredentials')}
                    </div>
                  ) : filteredLocalCredentials.map(entry => (
                    <div key={entry.id} className="rounded-3xl border border-border/70 bg-surface-sunken p-4">
                      <div className="flex items-start justify-between gap-3">
                        <div className="min-w-0">
                          <div className="text-sm font-semibold">{entry.label}</div>
                          <div className="mt-1 text-xs text-muted-foreground">
                            {entry.providerName}
                            {entry.baseUrl ? ` · ${baseUrlForProviderInput(entry.providerKind, entry.baseUrl)}` : ''}
                          </div>
                        </div>
                        <span className={`status-badge ${badgeClass(entry.state === 'active' ? 'ready' : entry.state === 'invalid' ? 'failed' : 'warning')}`}>
                          {credentialStateLabel(entry.state)}
                        </span>
                      </div>
                      <div className="mt-3 flex flex-wrap items-center gap-2 text-xs text-muted-foreground">
                        <Badge variant="outline">{scopeLabel(entry.scopeKind)}</Badge>
                        <span className="font-mono">{entry.apiKeySummary || t('admin.aiPanel.tokenOptional')}</span>
                      </div>
                      <div className="mt-4 flex justify-end">
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
              </ScrollArea>
            </div>

            <div className="workbench-surface overflow-hidden p-5">
              <div className="flex flex-col gap-4 sm:flex-row sm:items-start sm:justify-between">
                <div>
                  <div className="flex items-center gap-2">
                    <h3 className="text-sm font-bold tracking-tight">{t('admin.aiPanel.presetsTitle')}</h3>
                    <Badge variant="outline">{localPresets.length}</Badge>
                  </div>
                  <p className="mt-1 text-sm text-muted-foreground">{t('admin.aiPanel.presetsDescription')}</p>
                </div>
                <div className="flex w-full flex-col gap-2 sm:w-[260px]">
                  <div className="relative">
                    <Search className="pointer-events-none absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-muted-foreground" />
                    <Input className="pl-9" value={presetSearch} onChange={event => setPresetSearch(event.target.value)} placeholder={t('admin.aiPanel.filters.presetsSearch')} />
                  </div>
                  <Button size="sm" variant="outline" onClick={() => setPresetOpen(true)}>
                    <Brain className="mr-1.5 h-3.5 w-3.5" /> {t('admin.aiPanel.actions.addPreset')}
                  </Button>
                </div>
              </div>
              <ScrollArea className="mt-4 h-[420px] pr-4">
                <div className="space-y-3">
                  {localPresets.length === 0 ? (
                    <div className="rounded-2xl border border-dashed border-border/70 p-4 text-sm text-muted-foreground">
                      {t('admin.aiPanel.empty.noLocalPresets')}
                    </div>
                  ) : filteredLocalPresets.length === 0 ? (
                    <div className="rounded-2xl border border-dashed border-border/70 p-4 text-sm text-muted-foreground">
                      {t('admin.aiPanel.empty.noMatchingPresets')}
                    </div>
                  ) : filteredLocalPresets.map(entry => {
                    const model = modelById.get(entry.modelCatalogId);
                    const presetModelUnavailable = model?.availabilityState === 'unavailable';
                    return (
                      <div key={entry.id} className="rounded-3xl border border-border/70 bg-surface-sunken p-4">
                        <div className="flex items-start justify-between gap-3">
                          <div className="min-w-0">
                            <div className="text-sm font-semibold">{entry.presetName}</div>
                            <div className="mt-1 text-xs text-muted-foreground">{entry.providerName} · {entry.modelName}</div>
                          </div>
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
                        <div className="mt-3 flex flex-wrap gap-2">
                          {entry.allowedBindingPurposes.map(purpose => (
                            <Badge key={`${entry.id}-${purpose}`} variant="outline">{purposeLabel(purpose)}</Badge>
                          ))}
                        </div>
                        {presetModelUnavailable && (
                          <div className="mt-3 flex items-center gap-2 text-sm text-status-warning">
                            <AlertTriangle className="h-4 w-4" />
                            {t('admin.aiPanel.messages.presetModelUnavailable', { model: entry.modelName })}
                          </div>
                        )}
                      </div>
                    );
                  })}
                </div>
              </ScrollArea>
            </div>
          </div>
        </>
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
