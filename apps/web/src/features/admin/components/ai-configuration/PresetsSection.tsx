import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { AlertTriangle, Brain, Loader2, Pencil, Trash2 } from 'lucide-react';
import { toast } from 'sonner';
import { z } from 'zod';

import { adminApi } from '@/shared/api';
import { Badge } from '@/shared/components/ui/badge';
import { Button } from '@/shared/components/ui/button';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/shared/components/ui/dialog';
import { Label } from '@/shared/components/ui/label';
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/shared/components/ui/select';
import { errorMessage } from '@/shared/lib/errorMessage';
import type { AIModelOption, AIProvider, AIScopeKind, ModelPreset } from '@/shared/types';
import {
  compareByUpdatedAtDesc,
  formatModelLabel,
  localScopeQuery,
  matchesFilter,
  purposeLabel,
  type AiConfigDataState,
  type AiScopeContext,
} from '@/features/admin/model/aiConfig';
import {
  FormInputField,
  FormSelectField,
  FormTextareaField,
  nonEmptyString,
  optionalIntegerString,
  optionalNumberString,
  useTypedForm,
} from '@/shared/forms';

import { EntityWorkbench, InspectorField, InspectorSection } from './EntityWorkbench';

type PresetsSectionProps = {
  selectedScope: AIScopeKind;
  scopeContext: AiScopeContext;
  providers: AIProvider[];
  models: AIModelOption[];
  presetsState: AiConfigDataState<ModelPreset[]>;
  modelById: Map<string, AIModelOption>;
  invalidateAll: () => void;
  openAddRequest?: number;
};

export function PresetsSection({
  selectedScope,
  scopeContext,
  providers,
  models,
  presetsState,
  modelById,
  invalidateAll,
  openAddRequest = 0,
}: PresetsSectionProps) {
  const { t } = useTranslation();
  const [providerOverrideId, setProviderOverrideId] = useState('');
  const [presetOpen, setPresetOpen] = useState(false);
  const [editingPreset, setEditingPreset] = useState<ModelPreset | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<ModelPreset | null>(null);
  const [deletingPreset, setDeletingPreset] = useState(false);
  const [presetNameTouched, setPresetNameTouched] = useState(false);
  const [presetMaxTokensTouched, setPresetMaxTokensTouched] = useState(false);
  const lastOpenAddRequestRef = useRef(0);
  const presetSchema = useMemo(
    () =>
      z.object({
        maxTokens: optionalIntegerString(t('admin.maxOutputTokens')),
        modelId: nonEmptyString(t('admin.model')),
        name: nonEmptyString(t('admin.presetName')),
        systemPrompt: z.string().transform(value => value.trim()),
        temperature: optionalNumberString(t('admin.temperature')),
        topP: optionalNumberString(t('admin.topP')),
      }),
    [t],
  );
  const presetForm = useTypedForm({
    schema: presetSchema,
    defaultValues: {
      maxTokens: '',
      modelId: '',
      name: '',
      systemPrompt: '',
      temperature: '',
      topP: '',
    },
    mode: 'onChange',
  });
  const presetModelId = presetForm.watch('modelId');
  const { reset: resetPresetForm, setValue: setPresetValue } = presetForm;

  const localPresets = useMemo(() => presetsState.data ?? [], [presetsState.data]);
  const providerOptions = useMemo(
    () =>
      providers
        .slice()
        .sort(
          (left, right) =>
            left.displayName.localeCompare(right.displayName) || left.kind.localeCompare(right.kind),
        ),
    [providers],
  );
  const providerPresetCounts = useMemo(() => {
    const counts = new Map<string, number>();
    for (const preset of localPresets) {
      counts.set(preset.providerId, (counts.get(preset.providerId) ?? 0) + 1);
    }
    return counts;
  }, [localPresets]);
  const defaultProviderId = useMemo(() => {
    const providerWithPresets =
      providerOptions.find(provider => (providerPresetCounts.get(provider.id) ?? 0) > 0) ??
      providerOptions[0];
    return providerWithPresets?.id ?? '';
  }, [providerOptions, providerPresetCounts]);
  const selectedProviderId = useMemo(
    () =>
      providerOptions.some(provider => provider.id === providerOverrideId)
        ? providerOverrideId
        : defaultProviderId,
    [defaultProviderId, providerOptions, providerOverrideId],
  );
  const selectedProvider = useMemo(
    () => providerOptions.find(provider => provider.id === selectedProviderId) ?? null,
    [providerOptions, selectedProviderId],
  );
  const providerPresets = useMemo(
    () =>
      localPresets
        .filter(entry => entry.providerId === selectedProviderId)
        .slice()
        .sort(compareByUpdatedAtDesc),
    [localPresets, selectedProviderId],
  );
  const handleProviderChange = (providerId: string) => {
    setProviderOverrideId(providerId);
  };

  const selectableModels = useMemo(
    () =>
      models.filter(entry => {
        if (editingPreset && entry.id === presetModelId) {
          return true;
        }
        if (!selectedProviderId || entry.providerCatalogId !== selectedProviderId) {
          return false;
        }
        return entry.availabilityState !== 'unavailable';
      }),
    [editingPreset, models, presetModelId, selectedProviderId],
  );
  const addPresetDisabled = selectableModels.length === 0;
  const addPresetDisabledReasonId = 'admin-presets-add-disabled-reason';
  const defaultPresetName = useCallback(
    (model: AIModelOption | null | undefined) =>
      model
        ? t('admin.aiPanel.placeholders.presetNameForModel', {
            model: formatModelLabel(model, providers),
          })
        : '',
    [providers, t],
  );

  const resetPresetDialog = () => {
    setPresetOpen(false);
    setEditingPreset(null);
    setPresetNameTouched(false);
    setPresetMaxTokensTouched(false);
    resetPresetForm({
      maxTokens: '',
      modelId: '',
      name: '',
      systemPrompt: '',
      temperature: '',
      topP: '',
    });
  };

  const openNewPresetEditor = useCallback(() => {
    const firstModel = selectableModels[0];
    if (!firstModel) {
      return;
    }
    setEditingPreset(null);
    setPresetNameTouched(false);
    setPresetMaxTokensTouched(false);
    resetPresetForm({
      maxTokens: firstModel?.maxOutputTokens ? String(firstModel.maxOutputTokens) : '',
      modelId: firstModel?.id ?? '',
      name: defaultPresetName(firstModel),
      systemPrompt: '',
      temperature: '',
      topP: '',
    });
    setPresetOpen(true);
  }, [defaultPresetName, resetPresetForm, selectableModels]);

  useEffect(() => {
    if (openAddRequest <= 0 || lastOpenAddRequestRef.current === openAddRequest) {
      return;
    }
    lastOpenAddRequestRef.current = openAddRequest;
    openNewPresetEditor();
  }, [openAddRequest, openNewPresetEditor]);

  const openPresetEditor = (entry: ModelPreset) => {
    setProviderOverrideId(entry.providerId);
    setEditingPreset(entry);
    setPresetNameTouched(true);
    setPresetMaxTokensTouched(true);
    resetPresetForm({
      maxTokens: entry.maxOutputTokens !== undefined ? String(entry.maxOutputTokens) : '',
      modelId: entry.modelCatalogId,
      name: entry.presetName,
      systemPrompt: entry.systemPrompt ?? '',
      temperature: entry.temperature !== undefined ? String(entry.temperature) : '',
      topP: entry.topP !== undefined ? String(entry.topP) : '',
    });
    setPresetOpen(true);
  };

  const handlePresetModelChange = (modelId: string) => {
    const model = models.find(entry => entry.id === modelId) ?? null;
    if (!editingPreset && !presetNameTouched) {
      setPresetValue('name', defaultPresetName(model), {
        shouldDirty: true,
        shouldValidate: true,
      });
    }
    if (!editingPreset && !presetMaxTokensTouched) {
      setPresetValue('maxTokens', model?.maxOutputTokens ? String(model.maxOutputTokens) : '', {
        shouldDirty: true,
        shouldValidate: true,
      });
    }
  };

  const savePreset = presetForm.submitWithMutation(
    {
      mutateAsync: async values => {
        if (editingPreset) {
          await adminApi.updateModelPreset(editingPreset.id, {
            presetName: values.name.trim(),
            systemPrompt: values.systemPrompt || null,
            temperature: values.temperature,
            topP: values.topP,
            maxOutputTokensOverride: values.maxTokens,
            extraParametersJson: editingPreset.extraParams ?? {},
          });
        } else {
          const localParams = localScopeQuery(selectedScope, scopeContext);
          await adminApi.createModelPreset({
            ...(localParams.query ?? {}),
            modelCatalogId: values.modelId,
            presetName: values.name.trim(),
            systemPrompt: values.systemPrompt || null,
            temperature: values.temperature,
            topP: values.topP,
            maxOutputTokensOverride: values.maxTokens,
            extraParametersJson: {},
          });
        }
        resetPresetDialog();
        invalidateAll();
        toast.success(t('admin.aiPanel.messages.presetSaved'));
      },
    },
    {
      errorMessage: t('admin.aiPanel.messages.presetSaveFailed'),
    },
  );

  const deletePreset = async () => {
    if (!deleteTarget || deletingPreset) return;
    setDeletingPreset(true);
    try {
      await adminApi.deleteModelPreset(deleteTarget.id);
      setDeleteTarget(null);
      invalidateAll();
      toast.success(t('admin.aiPanel.messages.presetDeleted'));
    } catch (err) {
      toast.error(errorMessage(err, t('admin.aiPanel.messages.presetDeleteFailed')));
    } finally {
      setDeletingPreset(false);
    }
  };

  const toolbar = (
    <>
      <div className="w-[200px]">
        <Label htmlFor="admin-presets-provider" className="sr-only">
          {t('admin.aiPanel.fields.provider')}
        </Label>
        <Select
          value={selectedProviderId}
          onValueChange={handleProviderChange}
          disabled={providerOptions.length === 0}
        >
          <SelectTrigger id="admin-presets-provider" className="h-9">
            <SelectValue placeholder={t('admin.aiPanel.placeholders.selectProvider')} />
          </SelectTrigger>
          <SelectContent>
            {providerOptions.map(provider => (
              <SelectItem key={provider.id} value={provider.id}>
                {provider.displayName} · {providerPresetCounts.get(provider.id) ?? 0}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>
      <Button
        size="sm"
        onClick={openNewPresetEditor}
        disabled={addPresetDisabled}
        aria-describedby={addPresetDisabled ? addPresetDisabledReasonId : undefined}
      >
        <Brain className="mr-1.5 h-3.5 w-3.5" /> {t('admin.aiPanel.actions.addPreset')}
      </Button>
    </>
  );

  return (
    <div className="flex flex-1 min-h-0 flex-col">
      <EntityWorkbench<ModelPreset>
        tableId="admin.ai.presets"
        title={t('admin.aiPanel.presetsTitle')}
        count={providerPresets.length}
        state={presetsState}
        rows={providerPresets}
        rowKey={preset => preset.id}
        emptyMessage={
          selectedProvider
            ? t('admin.aiPanel.empty.noProviderPresets', { provider: selectedProvider.displayName })
            : t('admin.aiPanel.empty.noLocalPresets')
        }
        searchPlaceholder={t('admin.aiPanel.filters.presetsSearch')}
        toolbar={toolbar}
        rowActions={preset => (
          <>
            <Button
              type="button"
              size="icon"
              variant="ghost"
              className="h-8 w-8"
              aria-label={`${t('admin.edit')}: ${preset.presetName}`}
              onClick={() => openPresetEditor(preset)}
            >
              <Pencil className="h-3.5 w-3.5" />
            </Button>
            <Button
              type="button"
              size="icon"
              variant="ghost"
              className="h-8 w-8 text-status-failed hover:text-status-failed"
              aria-label={`${t('admin.delete')}: ${preset.presetName}`}
              onClick={() => setDeleteTarget(preset)}
            >
              <Trash2 className="h-3.5 w-3.5" />
            </Button>
          </>
        )}
        matchesFilter={(preset, filter) =>
          matchesFilter(
            [
              preset.presetName,
              preset.providerName,
              preset.providerKind,
              preset.modelName,
              preset.allowedBindingPurposes.map(p => purposeLabel(p, t)).join(', '),
            ],
            filter,
          )
        }
        columns={[
          {
            key: 'preset',
            header: t('admin.presetName'),
            sortValue: preset => preset.presetName,
            cell: preset => {
              const model = modelById.get(preset.modelCatalogId);
              const unavailable = model?.availabilityState === 'unavailable';
              return (
                <div className="min-w-0">
                  <div
                    className={`truncate text-sm font-semibold ${unavailable ? 'text-muted-foreground line-through' : ''}`}
                    title={preset.presetName}
                  >
                    {preset.presetName}
                  </div>
                  <div className="truncate text-[11px] text-muted-foreground">
                    {preset.providerName} · {preset.modelName}
                  </div>
                </div>
              );
            },
          },
          {
            key: 'purposes',
            header: t('admin.aiPanel.fields.purposes'),
            sortValue: preset => preset.allowedBindingPurposes.map(p => purposeLabel(p, t)).join(', '),
            cell: preset => {
              const labels = preset.allowedBindingPurposes.map(p => purposeLabel(p, t));
              const text = labels.length === 0 ? '—' : labels.join(', ');
              return (
                <span className="block truncate text-xs text-muted-foreground" title={text}>
                  {text}
                </span>
              );
            },
          },
          {
            key: 'temperature',
            header: t('admin.temperature'),
            width: 'w-24',
            align: 'right',
            sortValue: preset => preset.temperature ?? null,
            cell: preset => (
              <span className="tabular-nums text-xs text-muted-foreground">
                {preset.temperature ?? '—'}
              </span>
            ),
          },
        ]}
        renderInspector={preset => {
          const model = modelById.get(preset.modelCatalogId);
          const unavailable = model?.availabilityState === 'unavailable';
          return {
            row: preset,
            title: preset.presetName,
            subtitle: `${preset.providerName} · ${preset.modelName}`,
            body: (
              <>
                {unavailable && (
                  <div className="flex items-start gap-2 rounded-md border border-status-warning/25 bg-status-warning/5 p-3 text-sm text-status-warning">
                    <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0" />
                    <span>
                      {t('admin.aiPanel.messages.presetModelUnavailable', { model: preset.modelName })}
                    </span>
                  </div>
                )}
                <InspectorSection title={t('admin.aiPanel.fields.purposes')}>
                  <div className="flex flex-wrap gap-1.5">
                    {preset.allowedBindingPurposes.length === 0 ? (
                      <span className="text-xs text-muted-foreground">—</span>
                    ) : (
                      preset.allowedBindingPurposes.map(purpose => (
                        <Badge key={purpose} variant="outline">
                          {purposeLabel(purpose, t)}
                        </Badge>
                      ))
                    )}
                  </div>
                </InspectorSection>
                <InspectorSection title={t('admin.aiPanel.fields.parameters')}>
                  <InspectorField
                    label={t('admin.temperature')}
                    value={preset.temperature ?? '—'}
                  />
                  <InspectorField label={t('admin.topP')} value={preset.topP ?? '—'} />
                  <InspectorField
                    label={t('admin.maxOutputTokens')}
                    value={preset.maxOutputTokens?.toLocaleString() ?? '—'}
                  />
                </InspectorSection>
                {preset.systemPrompt && (
                  <InspectorSection title={t('admin.systemPrompt')}>
                    <div className="rounded-md border border-border/70 bg-surface-sunken p-3 text-xs whitespace-pre-wrap [overflow-wrap:anywhere]">
                      {preset.systemPrompt}
                    </div>
                  </InspectorSection>
                )}
                <InspectorSection title={t('admin.aiPanel.fields.identifier')}>
                  <InspectorField label={t('admin.aiPanel.fields.identifier')} value={preset.id} mono />
                </InspectorSection>
              </>
            ),
            actions: (
              <div className="grid gap-2 sm:grid-cols-2">
                <Button
                  size="sm"
                  variant="outline"
                  onClick={() => openPresetEditor(preset)}
                >
                  <Pencil className="mr-1.5 h-3.5 w-3.5" /> {t('admin.edit')}
                </Button>
                <Button
                  size="sm"
                  variant="outline"
                  onClick={() => setDeleteTarget(preset)}
                >
                  <Trash2 className="mr-1.5 h-3.5 w-3.5" /> {t('admin.delete')}
                </Button>
              </div>
            ),
          };
        }}
      />
      {addPresetDisabled && (
        <p id={addPresetDisabledReasonId} className="px-1 text-xs leading-relaxed text-status-warning">
          {t('admin.aiPanel.empty.noSelectableModels')}
        </p>
      )}

      <Dialog
        open={presetOpen}
        onOpenChange={open => {
          if (!open) resetPresetDialog();
          else setPresetOpen(true);
        }}
      >
        <DialogContent className="max-w-lg">
          <DialogHeader>
            <DialogTitle>
              {editingPreset
                ? t('admin.aiPanel.dialogs.editPresetTitle')
                : t('admin.aiPanel.dialogs.addPresetTitle')}
            </DialogTitle>
            <DialogDescription>{t('admin.aiPanel.dialogs.presetDescription')}</DialogDescription>
          </DialogHeader>
          <div className="space-y-4">
            <FormInputField
              formState={presetForm.formState}
              id="admin-preset-name"
              label={t('admin.presetName')}
              name="name"
              onValueChange={() => setPresetNameTouched(true)}
              registration={presetForm.register('name')}
              placeholder={t('admin.aiPanel.placeholders.presetName')}
            />
            <div>
              <FormSelectField
                control={presetForm.control}
                disabled={Boolean(editingPreset)}
                formState={presetForm.formState}
                id="admin-preset-model"
                label={t('admin.model')}
                name="modelId"
                onValueChange={handlePresetModelChange}
                placeholder={t('admin.aiPanel.placeholders.selectModel')}
              >
                {selectableModels.map(entry => (
                  <SelectItem key={entry.id} value={entry.id}>
                    {formatModelLabel(entry, providers)}
                  </SelectItem>
                ))}
              </FormSelectField>
              {selectableModels.length === 0 && (
                <p className="mt-2 text-xs text-status-warning">
                  {t('admin.aiPanel.empty.noSelectableModels')}
                </p>
              )}
            </div>
            <FormTextareaField
              formState={presetForm.formState}
              id="admin-preset-system-prompt"
              label={t('admin.systemPrompt')}
              name="systemPrompt"
              registration={presetForm.register('systemPrompt')}
              placeholder={t('admin.aiPanel.placeholders.systemPrompt')}
              textareaClassName="min-h-[120px]"
            />
            <div className="grid gap-4 sm:grid-cols-2">
              <FormInputField
                formState={presetForm.formState}
                id="admin-preset-temperature"
                label={t('admin.temperature')}
                name="temperature"
                registration={presetForm.register('temperature')}
                placeholder={t('admin.aiPanel.placeholders.defaultValue')}
              />
              <FormInputField
                formState={presetForm.formState}
                id="admin-preset-top-p"
                label={t('admin.topP')}
                name="topP"
                registration={presetForm.register('topP')}
                placeholder={t('admin.aiPanel.placeholders.defaultValue')}
              />
            </div>
            <FormInputField
              formState={presetForm.formState}
              id="admin-preset-max-tokens"
              label={t('admin.maxOutputTokens')}
              name="maxTokens"
              onValueChange={() => setPresetMaxTokensTouched(true)}
              registration={presetForm.register('maxTokens')}
              placeholder={t('admin.aiPanel.placeholders.defaultValue')}
            />
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={resetPresetDialog}>
              {t('admin.cancel')}
            </Button>
            <Button
              disabled={!presetForm.formState.isValid || presetForm.formState.isSubmitting}
              onClick={() => void savePreset()}
            >
              {presetForm.formState.isSubmitting ? (
                <>
                  <Loader2 className="mr-1.5 h-3.5 w-3.5 animate-spin" /> {t('admin.saving')}
                </>
              ) : (
                t('admin.save')
              )}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog open={Boolean(deleteTarget)} onOpenChange={open => { if (!open) setDeleteTarget(null); }}>
        <DialogContent className="max-w-md">
          <DialogHeader>
            <DialogTitle>{t('admin.aiPanel.dialogs.deletePresetTitle')}</DialogTitle>
            <DialogDescription>
              {t('admin.aiPanel.dialogs.deletePresetDescription', {
                preset: deleteTarget?.presetName ?? '',
              })}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="outline" onClick={() => setDeleteTarget(null)}>
              {t('admin.cancel')}
            </Button>
            <Button variant="destructive" disabled={deletingPreset} onClick={() => void deletePreset()}>
              {deletingPreset ? (
                <>
                  <Loader2 className="mr-1.5 h-3.5 w-3.5 animate-spin" /> {t('admin.saving')}
                </>
              ) : (
                t('admin.delete')
              )}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
