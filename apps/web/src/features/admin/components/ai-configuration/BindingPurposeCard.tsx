import { useTranslation } from 'react-i18next';
import { AlertTriangle, Brain, KeyRound, Loader2, Settings2 } from 'lucide-react';

import { Button } from '@/shared/components/ui/button';
import { Label } from '@/shared/components/ui/label';
import { SearchableSelect } from '@/shared/components/ui/searchable-select';
import type { AICredential, AIModelOption, AIPurpose, AIScopeKind, ModelPreset } from '@/shared/types';
import {
  badgeClass,
  formatPresetLabel,
  isModelAvailableForCredential,
  purposeLabel as translatedPurposeLabel,
  scopeLabel as translatedScopeLabel,
  type BindingResolution,
  type CredentialModelLoadState,
} from '@/features/admin/model/aiConfig';
import { shouldRefreshCredentialModels } from '@/shared/lib/ai-provider';

type BindingPurposeCardProps = {
  purpose: AIPurpose;
  selectedScope: AIScopeKind;
  resolved: BindingResolution;
  availableCredentials: AICredential[];
  availablePresets: ModelPreset[];
  modelById: Map<string, AIModelOption>;
  modelsByCredentialId: Record<string, AIModelOption[]>;
  selectedBindingCredential: AICredential | null;
  selectedBindingCredentialLoadState: CredentialModelLoadState | undefined;
  editing: boolean;
  bindingCredentialId: string;
  bindingPresetId: string;
  bindingSaving: boolean;
  onCredentialChange: (value: string) => void;
  onPresetChange: (value: string) => void;
  onOpen: () => void;
  onCancel: () => void;
  onSave: () => void;
  onReset: () => void;
};

export function BindingPurposeCard({
  purpose,
  selectedScope,
  resolved,
  availableCredentials,
  availablePresets,
  modelById,
  modelsByCredentialId,
  selectedBindingCredential,
  selectedBindingCredentialLoadState,
  editing,
  bindingCredentialId,
  bindingPresetId,
  bindingSaving,
  onCredentialChange,
  onPresetChange,
  onOpen,
  onCancel,
  onSave,
  onReset,
}: BindingPurposeCardProps) {
  const { t } = useTranslation();
  const purposeLabel = (value: AIPurpose) => translatedPurposeLabel(value, t);
  const scopeLabel = (value: AIScopeKind) => translatedScopeLabel(value, t);
  const credential = availableCredentials.find(entry => entry.id === resolved.effectiveBinding?.credentialId);
  const preset = availablePresets.find(entry => entry.id === resolved.effectiveBinding?.presetId);
  const presetModel = preset ? modelById.get(preset.modelCatalogId) : undefined;
  const bindingModelUnavailable =
    credential && preset
      ? !isModelAvailableForCredential(presetModel, credential, modelsByCredentialId)
      : false;
  const selectedCredentialModels = selectedBindingCredential
    ? modelsByCredentialId[selectedBindingCredential.id]
    : undefined;
  const selectedCredentialModelSet = selectedCredentialModels
    ? new Set(selectedCredentialModels.map(entry => entry.id))
    : null;
  const selectedCredentialRequiresModelDiscovery =
    shouldRefreshCredentialModels(selectedBindingCredential?.provider);
  const selectedCredentialModelDiscoveryPending =
    selectedCredentialRequiresModelDiscovery && selectedBindingCredentialLoadState !== 'ready';
  const presetOptions = availablePresets
    .filter(entry => entry.allowedBindingPurposes.includes(purpose))
    .filter(entry => !selectedBindingCredential || entry.providerId === selectedBindingCredential.providerId);
  const selectedPreset = availablePresets.find(entry => entry.id === bindingPresetId);
  const selectedPresetUnavailable =
    selectedBindingCredential !== null
    && bindingPresetId !== ''
    && (
      selectedCredentialModelDiscoveryPending
      || !isModelAvailableForCredential(
        modelById.get(selectedPreset?.modelCatalogId ?? ''),
        selectedBindingCredential,
        modelsByCredentialId,
      )
    );
  const showSourceBadge = resolved.sourceKind != null && resolved.sourceKind !== selectedScope;
  const actionLabel = resolved.localBinding
    ? t('admin.aiPanel.actions.editHere')
    : selectedScope === 'instance'
      ? t('admin.aiPanel.actions.setDefault')
      : t('admin.aiPanel.actions.createOverride');

  return (
    <div className="border-b border-border/70 bg-card px-3 py-3 last:border-b-0">
      <div className="grid grid-cols-[minmax(0,1fr)_auto] gap-x-3 gap-y-2 lg:grid-cols-[minmax(150px,0.65fr)_minmax(0,1.35fr)_auto] lg:items-center">
        <div className="min-w-0">
          <div className="flex flex-wrap items-center gap-2">
            <h4 className="truncate text-sm font-semibold tracking-tight">{purposeLabel(purpose)}</h4>
          </div>
          {showSourceBadge && resolved.sourceKind && (
            <span className={`mt-1 inline-flex status-badge text-[10px] ${badgeClass(resolved.sourceKind === 'instance' ? 'ready' : 'warning')}`}>
              {t('admin.aiPanel.labels.inheritedFrom', { scope: scopeLabel(resolved.sourceKind) })}
            </span>
          )}
        </div>

        {resolved.effectiveBinding && credential && preset ? (
          <div className="col-span-2 min-w-0 space-y-0.5 text-sm lg:col-span-1">
            <div className="flex items-center gap-2">
              <KeyRound className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
              <span className="min-w-0 font-semibold [overflow-wrap:anywhere]">{credential.label}</span>
              <span className="min-w-0 text-xs text-muted-foreground [overflow-wrap:anywhere]">· {credential.providerName}</span>
            </div>
            <div className="flex items-center gap-2">
              <Brain className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
              <span className="min-w-0 font-semibold [overflow-wrap:anywhere]">{formatPresetLabel(preset)}</span>
              <span className="min-w-0 text-xs text-muted-foreground [overflow-wrap:anywhere]">· {preset.modelName}</span>
            </div>
          </div>
        ) : (
          <div className="col-span-2 flex min-w-0 items-center gap-2 text-sm text-status-warning lg:col-span-1">
            <AlertTriangle className="h-4 w-4 shrink-0" />
            <span>{t('admin.aiPanel.empty.noEffectiveBinding')}</span>
          </div>
        )}

        <Button
          size="sm"
          variant="outline"
          className="col-start-2 row-start-1 min-w-0 justify-center lg:col-start-auto lg:row-start-auto"
          aria-label={actionLabel}
          title={actionLabel}
          onClick={onOpen}
        >
          <Settings2 className="mr-1.5 h-3.5 w-3.5" />
          <span className="truncate">{actionLabel}</span>
        </Button>
      </div>

      {bindingModelUnavailable && (
        <div className="mt-3 flex items-center gap-2 rounded-md border border-status-warning/25 bg-status-warning/5 px-3 py-2 text-sm text-status-warning">
          <AlertTriangle className="h-4 w-4 shrink-0" />
          {t('admin.aiPanel.messages.bindingModelUnavailable', { model: preset?.modelName ?? '' })}
        </div>
      )}

      {editing && (
        <div className="mt-3 rounded-md border border-border/70 bg-surface-sunken p-3">
          <div className="grid gap-3 xl:grid-cols-2">
            <div>
              <Label className="text-xs font-semibold">{t('admin.aiPanel.fields.credential')}</Label>
              <div className="mt-2">
                <SearchableSelect
                  value={bindingCredentialId}
                  onValueChange={onCredentialChange}
                  placeholder={t('admin.aiPanel.placeholders.selectCredential')}
                  searchPlaceholder={t('admin.aiPanel.filters.credentialsSearch')}
                  options={availableCredentials.map(entry => ({
                    value: entry.id,
                    label: entry.label,
                    description: `${entry.providerName} · ${scopeLabel(entry.scopeKind)}`,
                    searchKeywords: `${entry.providerName} ${entry.providerKind} ${entry.scopeKind}`,
                  }))}
                />
              </div>
            </div>
            <div>
              <Label className="text-xs font-semibold">{t('admin.aiPanel.fields.modelPreset')}</Label>
              <div className="mt-2">
                <SearchableSelect
                  value={bindingPresetId}
                  onValueChange={onPresetChange}
                  placeholder={t('admin.aiPanel.placeholders.selectPreset')}
                  searchPlaceholder={t('admin.aiPanel.filters.presetsSearch')}
                  options={presetOptions.map(entry => {
                    const unavailable =
                      selectedCredentialModelSet !== null
                      && !selectedCredentialModelSet.has(entry.modelCatalogId);
                    return {
                      value: entry.id,
                      label: formatPresetLabel(entry),
                      description: unavailable
                        ? `${entry.modelName} · ${t('admin.aiPanel.unavailableBadge')}`
                        : entry.modelName,
                      searchKeywords: `${entry.providerName} ${entry.modelName}`,
                    };
                  })}
                />
              </div>
            </div>
          </div>

          {selectedBindingCredentialLoadState === 'loading' && (
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
          <div className="mt-3 flex flex-wrap gap-2">
            <Button size="sm" disabled={!bindingCredentialId || !bindingPresetId || bindingSaving || Boolean(selectedPresetUnavailable)} onClick={onSave}>
              {bindingSaving ? <><Loader2 className="mr-1.5 h-3.5 w-3.5 animate-spin" /> {t('admin.saving')}</> : t('admin.save')}
            </Button>
            <Button size="sm" variant="outline" onClick={onCancel}>{t('admin.cancel')}</Button>
            {resolved.localBinding && selectedScope !== 'instance' && (
              <Button size="sm" variant="outline" onClick={onReset}>
                {t('admin.aiPanel.actions.resetToInherited')}
              </Button>
            )}
          </div>
        </div>
      )}
    </div>
  );
}
