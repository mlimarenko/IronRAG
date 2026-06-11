import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Database, Loader2, Pencil, Plus, Trash2 } from 'lucide-react';
import { toast } from 'sonner';
import { z } from 'zod';

import { adminApi } from '@/shared/api';
import { Badge } from '@/shared/components/ui/badge';
import { Button } from '@/shared/components/ui/button';
import { Checkbox } from '@/shared/components/ui/checkbox';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/shared/components/ui/dialog';
import { Label } from '@/shared/components/ui/label';
import { SelectItem } from '@/shared/components/ui/select';
import { errorMessage } from '@/shared/lib/errorMessage';
import type { AIModelOption, AIProvider, AIPurpose } from '@/shared/types';
import {
  formatModelLabel,
  matchesFilter,
  purposeLabel,
  type AiConfigDataState,
} from '@/features/admin/model/aiConfig';
import {
  FormInputField,
  FormSelectField,
  FormTextareaField,
  nonEmptyString,
  optionalIntegerString,
  useTypedForm,
} from '@/shared/forms';

import { EntityWorkbench, InspectorField, InspectorSection } from './EntityWorkbench';

type ModelsSectionProps = {
  modelsState: AiConfigDataState<AIModelOption[]>;
  providers: AIProvider[];
  invalidateAll: () => void;
};

const MODEL_LIFECYCLE_STATES = ['active', 'preview', 'deprecated', 'disabled'] as const;
const MODEL_CAPABILITIES = ['chat', 'embedding'] as const;
const MODEL_MODALITIES = ['text', 'multimodal'] as const;
const MODEL_PURPOSES = [
  'extract_text',
  'extract_graph',
  'embed_chunk',
  'query_compile',
  'query_retrieve',
  'query_answer',
  'agent',
  'vision',
] as const satisfies readonly AIPurpose[];

type ModelEditorValues = {
  capabilityKind: (typeof MODEL_CAPABILITIES)[number];
  contextWindow: number | null;
  lifecycleState: (typeof MODEL_LIFECYCLE_STATES)[number];
  maxOutputTokens: number | null;
  metadataJson: string;
  modalityKind: (typeof MODEL_MODALITIES)[number];
  modelName: string;
  providerId: string;
  purposes: AIPurpose[];
};

function parseOptionalJsonObject(value: string) {
  const normalized = value.trim();
  if (!normalized) {
    return undefined;
  }
  const parsed = JSON.parse(normalized) as unknown;
  if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
    throw new Error('json');
  }
  return parsed as Record<string, unknown>;
}

function iconButtonLabel(action: string, title: string) {
  return `${action}: ${title}`;
}

export function ModelsSection({ modelsState, providers, invalidateAll }: ModelsSectionProps) {
  const { t } = useTranslation();
  const [editorOpen, setEditorOpen] = useState(false);
  const [editingModel, setEditingModel] = useState<AIModelOption | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<AIModelOption | null>(null);
  const [deleting, setDeleting] = useState(false);
  const providerById = useMemo(() => new Map(providers.map(p => [p.id, p])), [providers]);

  const modelSchema = useMemo(
    () =>
      z.object({
        capabilityKind: z.enum(MODEL_CAPABILITIES),
        contextWindow: optionalIntegerString(t('admin.aiPanel.fields.contextWindow')),
        lifecycleState: z.enum(MODEL_LIFECYCLE_STATES),
        maxOutputTokens: optionalIntegerString(t('admin.maxOutputTokens')),
        metadataJson: z.string().superRefine((value, context) => {
          try {
            parseOptionalJsonObject(value);
          } catch {
            context.addIssue({
              code: 'custom',
              message: t('admin.aiPanel.messages.invalidJson'),
            });
          }
        }),
        modalityKind: z.enum(MODEL_MODALITIES),
        modelName: nonEmptyString(t('admin.model')),
        providerId: nonEmptyString(t('admin.provider')),
        purposes: z.array(z.enum(MODEL_PURPOSES)).min(1, t('admin.aiPanel.messages.modelPurposeRequired')),
      }),
    [t],
  );

  const modelForm = useTypedForm({
    schema: modelSchema,
    defaultValues: {
      capabilityKind: 'chat',
      contextWindow: '',
      lifecycleState: 'active',
      maxOutputTokens: '',
      metadataJson: '',
      modalityKind: 'text',
      modelName: '',
      providerId: providers[0]?.id ?? '',
      purposes: ['query_answer'],
    },
    mode: 'onChange',
  });

  const selectedPurposes = modelForm.watch('purposes') ?? [];
  const models = modelsState.data ?? [];
  const addModelDisabled = providers.length === 0;

  const resetEditor = () => {
    setEditorOpen(false);
    setEditingModel(null);
    modelForm.reset({
      capabilityKind: 'chat',
      contextWindow: '',
      lifecycleState: 'active',
      maxOutputTokens: '',
      metadataJson: '',
      modalityKind: 'text',
      modelName: '',
      providerId: providers[0]?.id ?? '',
      purposes: ['query_answer'],
    });
  };

  const openNewModelEditor = () => {
    setEditingModel(null);
    modelForm.reset({
      capabilityKind: 'chat',
      contextWindow: '',
      lifecycleState: 'active',
      maxOutputTokens: '',
      metadataJson: '{}',
      modalityKind: 'text',
      modelName: '',
      providerId: providers[0]?.id ?? '',
      purposes: ['query_answer'],
    });
    setEditorOpen(true);
  };

  const openModelEditor = (model: AIModelOption) => {
    setEditingModel(model);
    modelForm.reset({
      capabilityKind: MODEL_CAPABILITIES.includes(model.capabilityKind as (typeof MODEL_CAPABILITIES)[number])
        ? (model.capabilityKind as (typeof MODEL_CAPABILITIES)[number])
        : 'chat',
      contextWindow: model.contextWindow != null ? String(model.contextWindow) : '',
      lifecycleState: model.lifecycleState ?? (model.availabilityState === 'unavailable' ? 'disabled' : 'active'),
      maxOutputTokens: model.maxOutputTokens != null ? String(model.maxOutputTokens) : '',
      metadataJson: '',
      modalityKind: MODEL_MODALITIES.includes(model.modalityKind as (typeof MODEL_MODALITIES)[number])
        ? (model.modalityKind as (typeof MODEL_MODALITIES)[number])
        : 'text',
      modelName: model.modelName,
      providerId: model.providerCatalogId,
      purposes: model.allowedBindingPurposes.length > 0 ? model.allowedBindingPurposes : ['query_answer'],
    });
    setEditorOpen(true);
  };

  const setPurpose = (purpose: AIPurpose, checked: boolean) => {
    const next = checked
      ? Array.from(new Set([...selectedPurposes, purpose]))
      : selectedPurposes.filter(entry => entry !== purpose);
    modelForm.setValue('purposes', next, { shouldDirty: true, shouldValidate: true });
  };

  const saveModel = modelForm.submitWithMutation(
    {
      mutateAsync: async (values: ModelEditorValues) => {
        const metadataJson = parseOptionalJsonObject(values.metadataJson);
        const body = {
          allowedBindingPurposes: values.purposes,
          capabilityKind: values.capabilityKind,
          contextWindow: values.contextWindow,
          lifecycleState: values.lifecycleState,
          maxOutputTokens: values.maxOutputTokens,
          modalityKind: values.modalityKind,
          modelName: values.modelName.trim(),
          providerCatalogId: values.providerId,
          ...(editingModel
            ? metadataJson !== undefined
              ? { metadataJson }
              : {}
            : { metadataJson: metadataJson ?? {} }),
        };
        if (editingModel) {
          await adminApi.updateModel(editingModel.id, body);
        } else {
          await adminApi.createModel(body);
        }
        resetEditor();
        invalidateAll();
        toast.success(t('admin.aiPanel.messages.modelSaved'));
      },
    },
    {
      errorMessage: t('admin.aiPanel.messages.modelSaveFailed'),
    },
  );

  const disableModel = async () => {
    if (!deleteTarget || deleting) return;
    setDeleting(true);
    try {
      await adminApi.deleteModel(deleteTarget.id);
      setDeleteTarget(null);
      invalidateAll();
      toast.success(t('admin.aiPanel.messages.modelDisabled'));
    } catch (err) {
      toast.error(errorMessage(err, t('admin.aiPanel.messages.modelDisableFailed')));
    } finally {
      setDeleting(false);
    }
  };

  const toolbar = (
    <Button size="sm" onClick={openNewModelEditor} disabled={addModelDisabled}>
      <Plus className="mr-1.5 h-3.5 w-3.5" /> {t('admin.aiPanel.actions.addModel')}
    </Button>
  );

  const modelActions = (model: AIModelOption) => (
    <>
      <Button
        type="button"
        size="icon"
        variant="ghost"
        className="h-8 w-8"
        aria-label={iconButtonLabel(t('admin.edit'), model.modelName)}
        onClick={() => openModelEditor(model)}
      >
        <Pencil className="h-3.5 w-3.5" />
      </Button>
      <Button
        type="button"
        size="icon"
        variant="ghost"
        className="h-8 w-8 text-status-failed hover:text-status-failed"
        aria-label={iconButtonLabel(t('admin.aiPanel.actions.disableModel'), model.modelName)}
        onClick={() => setDeleteTarget(model)}
      >
        <Trash2 className="h-3.5 w-3.5" />
      </Button>
    </>
  );

  return (
    <div className="flex flex-1 min-h-0 flex-col">
      <EntityWorkbench<AIModelOption>
        tableId="admin.ai.models"
        title={t('admin.aiPanel.metrics.visibleModels')}
        count={models.length}
        state={modelsState}
        rows={models}
        rowKey={model => model.id}
        emptyMessage={t('admin.aiPanel.empty.noSelectableModels')}
        searchPlaceholder={t('admin.searchModels')}
        toolbar={toolbar}
        rowActions={modelActions}
        matchesFilter={(model, filter) =>
          matchesFilter(
            [
              model.modelName,
              model.capabilityKind,
              model.modalityKind,
              formatModelLabel(model, providers),
              model.allowedBindingPurposes.map(p => purposeLabel(p, t)).join(', '),
            ],
            filter,
          )
        }
        columns={[
          {
            key: 'name',
            header: t('admin.model'),
            sortValue: model => model.modelName,
            cell: model => {
              const provider = providerById.get(model.providerCatalogId);
              const unavailable = model.availabilityState === 'unavailable';
              return (
                <div className="min-w-0">
                  <div
                    className={`truncate text-sm font-semibold ${unavailable ? 'text-muted-foreground line-through' : ''}`}
                    title={model.modelName}
                  >
                    {model.modelName}
                  </div>
                  <div className="truncate text-[11px] text-muted-foreground">
                    {provider?.displayName ?? '—'}
                    {model.capabilityKind ? ` · ${model.capabilityKind}` : ''}
                  </div>
                </div>
              );
            },
          },
          {
            key: 'modality',
            header: t('admin.aiPanel.fields.modality'),
            width: 'w-32',
            sortValue: model => model.modalityKind,
            cell: model => (
              <span className="text-xs text-muted-foreground">{model.modalityKind || '—'}</span>
            ),
          },
          {
            key: 'purposes',
            header: t('admin.aiPanel.fields.purposes'),
            sortValue: model => model.allowedBindingPurposes.map(p => purposeLabel(p, t)).join(', '),
            cell: model => {
              const labels = model.allowedBindingPurposes.map(p => purposeLabel(p, t));
              const text = labels.length === 0 ? '—' : labels.join(', ');
              return (
                <span className="block truncate text-xs text-muted-foreground" title={text}>
                  {text}
                </span>
              );
            },
          },
        ]}
        renderInspector={model => {
          const provider = providerById.get(model.providerCatalogId);
          const unavailable = model.availabilityState === 'unavailable';
          return {
            row: model,
            title: model.modelName,
            subtitle: provider?.displayName,
            body: (
              <>
                {unavailable && (
                  <div className="rounded-md border border-status-warning/25 bg-status-warning/5 p-3 text-sm text-status-warning">
                    {t('admin.aiPanel.unavailableBadge')}
                  </div>
                )}
                <InspectorSection title={t('admin.aiPanel.fields.profile')}>
                  <InspectorField
                    label={t('admin.aiPanel.fields.capability')}
                    value={model.capabilityKind}
                    mono
                  />
                  <InspectorField
                    label={t('admin.aiPanel.fields.modality')}
                    value={model.modalityKind || '—'}
                  />
                  {model.contextWindow != null && (
                    <InspectorField
                      label={t('admin.aiPanel.fields.contextWindow')}
                      value={model.contextWindow.toLocaleString()}
                    />
                  )}
                  {model.maxOutputTokens != null && (
                    <InspectorField
                      label={t('admin.maxOutputTokens')}
                      value={model.maxOutputTokens.toLocaleString()}
                    />
                  )}
                </InspectorSection>
                <InspectorSection title={t('admin.aiPanel.fields.purposes')}>
                  <div className="flex flex-wrap gap-1.5">
                    {model.allowedBindingPurposes.length === 0 ? (
                      <span className="text-xs text-muted-foreground">—</span>
                    ) : (
                      model.allowedBindingPurposes.map(purpose => (
                        <Badge key={purpose} variant="outline">
                          {purposeLabel(purpose, t)}
                        </Badge>
                      ))
                    )}
                  </div>
                </InspectorSection>
                <InspectorSection title={t('admin.aiPanel.fields.identifier')}>
                  <InspectorField label={t('admin.aiPanel.fields.identifier')} value={model.id} mono />
                  <InspectorField
                    label={t('admin.aiPanel.fields.providerId')}
                    value={model.providerCatalogId}
                    mono
                  />
                </InspectorSection>
              </>
            ),
            actions: (
              <div className="grid gap-2 sm:grid-cols-2">
                <Button size="sm" variant="outline" onClick={() => openModelEditor(model)}>
                  <Pencil className="mr-1.5 h-3.5 w-3.5" /> {t('admin.edit')}
                </Button>
                <Button size="sm" variant="outline" onClick={() => setDeleteTarget(model)}>
                  <Trash2 className="mr-1.5 h-3.5 w-3.5" /> {t('admin.aiPanel.actions.disableModel')}
                </Button>
              </div>
            ),
          };
        }}
      />

      <Dialog
        open={editorOpen}
        onOpenChange={open => {
          if (!open) resetEditor();
          else setEditorOpen(true);
        }}
      >
        <DialogContent className="max-w-2xl">
          <DialogHeader>
            <DialogTitle>
              {editingModel
                ? t('admin.aiPanel.dialogs.editModelTitle')
                : t('admin.aiPanel.dialogs.addModelTitle')}
            </DialogTitle>
            <DialogDescription>{t('admin.aiPanel.dialogs.modelDescription')}</DialogDescription>
          </DialogHeader>
          <div className="grid gap-4 sm:grid-cols-2">
            <FormSelectField
              control={modelForm.control}
              formState={modelForm.formState}
              id="admin-model-provider"
              label={t('admin.provider')}
              name="providerId"
              placeholder={t('admin.aiPanel.placeholders.selectProvider')}
            >
              {providers.map(provider => (
                <SelectItem key={provider.id} value={provider.id}>
                  {provider.displayName}
                </SelectItem>
              ))}
            </FormSelectField>
            <FormInputField
              formState={modelForm.formState}
              id="admin-model-name"
              label={t('admin.model')}
              name="modelName"
              registration={modelForm.register('modelName')}
              placeholder={t('admin.aiPanel.placeholders.modelName')}
            />
            <FormSelectField
              control={modelForm.control}
              formState={modelForm.formState}
              id="admin-model-capability"
              label={t('admin.aiPanel.fields.capability')}
              name="capabilityKind"
              placeholder={t('admin.aiPanel.fields.capability')}
            >
              {MODEL_CAPABILITIES.map(value => (
                <SelectItem key={value} value={value}>
                  {t(`admin.aiPanel.capabilityLabels.${value}`)}
                </SelectItem>
              ))}
            </FormSelectField>
            <FormSelectField
              control={modelForm.control}
              formState={modelForm.formState}
              id="admin-model-modality"
              label={t('admin.aiPanel.fields.modality')}
              name="modalityKind"
              placeholder={t('admin.aiPanel.fields.modality')}
            >
              {MODEL_MODALITIES.map(value => (
                <SelectItem key={value} value={value}>
                  {t(`admin.aiPanel.modalityLabels.${value}`)}
                </SelectItem>
              ))}
            </FormSelectField>
            <FormSelectField
              control={modelForm.control}
              formState={modelForm.formState}
              id="admin-model-lifecycle"
              label={t('admin.aiPanel.fields.lifecycle')}
              name="lifecycleState"
              placeholder={t('admin.aiPanel.placeholders.lifecycle')}
            >
              {MODEL_LIFECYCLE_STATES.map(state => (
                <SelectItem key={state} value={state}>
                  {t(`admin.aiPanel.lifecycleLabels.${state}`)}
                </SelectItem>
              ))}
            </FormSelectField>
            <FormInputField
              formState={modelForm.formState}
              id="admin-model-context-window"
              label={t('admin.aiPanel.fields.contextWindow')}
              name="contextWindow"
              registration={modelForm.register('contextWindow')}
              placeholder={t('admin.aiPanel.placeholders.defaultValue')}
            />
            <FormInputField
              formState={modelForm.formState}
              id="admin-model-max-output"
              label={t('admin.maxOutputTokens')}
              name="maxOutputTokens"
              registration={modelForm.register('maxOutputTokens')}
              placeholder={t('admin.aiPanel.placeholders.defaultValue')}
            />
          </div>
          <div className="space-y-2">
            <Label>{t('admin.aiPanel.fields.purposes')}</Label>
            <div className="grid gap-2 sm:grid-cols-2">
              {MODEL_PURPOSES.map(purpose => (
                <label
                  key={purpose}
                  className="flex items-center gap-2 rounded-md border border-border/70 px-3 py-2 text-sm"
                >
                  <Checkbox
                    checked={selectedPurposes.includes(purpose)}
                    onCheckedChange={checked => setPurpose(purpose, checked === true)}
                  />
                  <span>{purposeLabel(purpose, t)}</span>
                </label>
              ))}
            </div>
            {modelForm.formState.errors.purposes && (
              <p className="text-xs text-status-failed">{modelForm.formState.errors.purposes.message}</p>
            )}
          </div>
          <FormTextareaField
            formState={modelForm.formState}
            id="admin-model-metadata"
            label={t('admin.aiPanel.fields.metadataJson')}
            name="metadataJson"
            registration={modelForm.register('metadataJson')}
            placeholder={editingModel ? t('admin.aiPanel.placeholders.keepModelMetadata') : t('admin.aiPanel.placeholders.emptyJson')}
            textareaClassName="min-h-[120px] font-mono text-xs"
          />
          <DialogFooter>
            <Button variant="outline" onClick={resetEditor}>
              {t('admin.cancel')}
            </Button>
            <Button
              disabled={!modelForm.formState.isValid || modelForm.formState.isSubmitting}
              onClick={() => void saveModel()}
            >
              {modelForm.formState.isSubmitting ? (
                <>
                  <Loader2 className="mr-1.5 h-3.5 w-3.5 animate-spin" /> {t('admin.saving')}
                </>
              ) : (
                <>
                  <Database className="mr-1.5 h-3.5 w-3.5" /> {t('admin.save')}
                </>
              )}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog open={Boolean(deleteTarget)} onOpenChange={open => { if (!open) setDeleteTarget(null); }}>
        <DialogContent className="max-w-md">
          <DialogHeader>
            <DialogTitle>{t('admin.aiPanel.dialogs.disableModelTitle')}</DialogTitle>
            <DialogDescription>
              {t('admin.aiPanel.dialogs.disableModelDescription', {
                model: deleteTarget?.modelName ?? '',
              })}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="outline" onClick={() => setDeleteTarget(null)}>
              {t('admin.cancel')}
            </Button>
            <Button variant="destructive" disabled={deleting} onClick={() => void disableModel()}>
              {deleting ? (
                <>
                  <Loader2 className="mr-1.5 h-3.5 w-3.5 animate-spin" /> {t('admin.saving')}
                </>
              ) : (
                t('admin.aiPanel.actions.disableModel')
              )}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
