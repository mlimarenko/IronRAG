import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Loader2, Pencil, Plus, Server, Trash2 } from 'lucide-react';
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
import { SelectItem } from '@/shared/components/ui/select';
import { errorMessage } from '@/shared/lib/errorMessage';
import type { AICredential, AIModelOption, AIProvider } from '@/shared/types';
import { badgeClass, matchesFilter, type AiConfigDataState } from '@/features/admin/model/aiConfig';
import {
  FormInputField,
  FormSelectField,
  FormTextareaField,
  nonEmptyString,
  useTypedForm,
} from '@/shared/forms';

import { EntityWorkbench, InspectorField, InspectorSection } from './EntityWorkbench';

type ProvidersSectionProps = {
  providersState: AiConfigDataState<AIProvider[]>;
  models: AIModelOption[];
  credentials: AICredential[];
  invalidateAll: () => void;
};

type ProviderEditorValues = {
  apiStyle: string;
  defaultBaseUrl: string;
  displayName: string;
  kind: string;
  lifecycleState: AIProvider['lifecycleState'];
  profileJson: string;
};

const PROVIDER_API_STYLE = 'openai_compatible';
const PROVIDER_LIFECYCLE_STATES = ['active', 'preview', 'deprecated', 'disabled'] as const;

const DEFAULT_PROVIDER_PROFILE = {
  runtime: {
    kind: PROVIDER_API_STYLE,
    authScheme: 'bearer',
    tokenLimitParameter: 'max_tokens',
    structuredOutput: 'json_schema',
    chatPath: '/chat/completions',
    embeddingsPath: '/embeddings',
    modelsPath: '/models',
  },
  credentials: {
    apiKeyRequired: true,
    baseUrlRequired: false,
    baseUrlMode: 'optional',
    validationMode: 'model_list',
  },
  baseUrl: {
    allowOverride: true,
    requireHttps: true,
    allowPrivateNetwork: false,
    trimSuffixes: [],
  },
  modelDiscovery: {
    mode: 'credential',
    paths: [
      { capabilityKind: 'chat', path: '/models' },
      { capabilityKind: 'embedding', path: '/models' },
    ],
  },
  capabilities: {
    chat: 'supported',
    embeddings: 'supported',
    vision: 'unknown',
    streaming: 'unknown',
    tools: 'unknown',
    modelDiscovery: 'supported',
  },
  uiHints: {},
};

function lifecycleTone(state: AIProvider['lifecycleState']) {
  return state === 'active'
    ? 'ready'
    : state === 'deprecated' || state === 'disabled'
      ? 'failed'
      : 'warning';
}

function stringifyJson(value: unknown) {
  return JSON.stringify(value, null, 2);
}

function parseJsonObject(value: string) {
  const parsed = JSON.parse(value) as unknown;
  if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
    throw new Error('json');
  }
  return parsed as Record<string, unknown>;
}

function profileFromProvider(provider: AIProvider) {
  return {
    runtime: provider.runtime,
    credentials: provider.credentialPolicy,
    baseUrl: provider.baseUrlPolicy,
    modelDiscovery: provider.modelDiscovery,
    capabilities: provider.capabilities,
    uiHints: provider.uiHints ?? {},
  };
}

function iconButtonLabel(action: string, title: string) {
  return `${action}: ${title}`;
}

export function ProvidersSection({
  providersState,
  models,
  credentials,
  invalidateAll,
}: ProvidersSectionProps) {
  const { t } = useTranslation();
  const [editorOpen, setEditorOpen] = useState(false);
  const [editingProvider, setEditingProvider] = useState<AIProvider | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<AIProvider | null>(null);
  const [deleting, setDeleting] = useState(false);

  const providerSchema = useMemo(
    () =>
      z.object({
        apiStyle: nonEmptyString(t('admin.aiPanel.fields.apiStyle')),
        defaultBaseUrl: z.string().transform(value => value.trim()),
        displayName: nonEmptyString(t('admin.aiPanel.fields.displayName')),
        kind: nonEmptyString(t('admin.aiPanel.fields.providerKind')),
        lifecycleState: z.enum(PROVIDER_LIFECYCLE_STATES),
        profileJson: z.string().superRefine((value, context) => {
          try {
            parseJsonObject(value);
          } catch {
            context.addIssue({
              code: 'custom',
              message: t('admin.aiPanel.messages.invalidJson'),
            });
          }
        }),
      }),
    [t],
  );

  const providerForm = useTypedForm({
    schema: providerSchema,
    defaultValues: {
      apiStyle: PROVIDER_API_STYLE,
      defaultBaseUrl: '',
      displayName: '',
      kind: '',
      lifecycleState: 'active',
      profileJson: stringifyJson(DEFAULT_PROVIDER_PROFILE),
    },
    mode: 'onChange',
  });

  const providers = useMemo(() => {
    const base = providersState.data ?? [];
    return base.map(provider => ({
      ...provider,
      modelCount: models.filter(model => model.providerCatalogId === provider.id).length,
      credentialCount: credentials.filter(credential => credential.providerId === provider.id).length,
    }));
  }, [credentials, models, providersState.data]);

  const resetEditor = () => {
    setEditorOpen(false);
    setEditingProvider(null);
    providerForm.reset({
      apiStyle: PROVIDER_API_STYLE,
      defaultBaseUrl: '',
      displayName: '',
      kind: '',
      lifecycleState: 'active',
      profileJson: stringifyJson(DEFAULT_PROVIDER_PROFILE),
    });
  };

  const openNewProviderEditor = () => {
    setEditingProvider(null);
    providerForm.reset({
      apiStyle: PROVIDER_API_STYLE,
      defaultBaseUrl: '',
      displayName: '',
      kind: '',
      lifecycleState: 'active',
      profileJson: stringifyJson(DEFAULT_PROVIDER_PROFILE),
    });
    setEditorOpen(true);
  };

  const openProviderEditor = (provider: AIProvider) => {
    setEditingProvider(provider);
    providerForm.reset({
      apiStyle: provider.apiStyle || PROVIDER_API_STYLE,
      defaultBaseUrl: provider.defaultBaseUrl ?? '',
      displayName: provider.displayName,
      kind: provider.kind,
      lifecycleState: provider.lifecycleState,
      profileJson: stringifyJson(profileFromProvider(provider)),
    });
    setEditorOpen(true);
  };

  const saveProvider = providerForm.submitWithMutation(
    {
      mutateAsync: async (values: ProviderEditorValues) => {
        const body = {
          apiStyle: values.apiStyle,
          defaultBaseUrl: values.defaultBaseUrl.trim() || null,
          displayName: values.displayName.trim(),
          lifecycleState: values.lifecycleState,
          providerKind: values.kind.trim(),
          capabilityFlagsJson: parseJsonObject(values.profileJson),
        };
        if (editingProvider) {
          await adminApi.updateProvider(editingProvider.id, body);
        } else {
          await adminApi.createProvider(body);
        }
        resetEditor();
        invalidateAll();
        toast.success(t('admin.aiPanel.messages.providerSaved'));
      },
    },
    {
      errorMessage: t('admin.aiPanel.messages.providerSaveFailed'),
    },
  );

  const disableProvider = async () => {
    if (!deleteTarget || deleting) return;
    setDeleting(true);
    try {
      await adminApi.deleteProvider(deleteTarget.id);
      setDeleteTarget(null);
      invalidateAll();
      toast.success(t('admin.aiPanel.messages.providerDisabled'));
    } catch (err) {
      toast.error(errorMessage(err, t('admin.aiPanel.messages.providerDisableFailed')));
    } finally {
      setDeleting(false);
    }
  };

  const toolbar = (
    <Button size="sm" onClick={openNewProviderEditor}>
      <Plus className="mr-1.5 h-3.5 w-3.5" /> {t('admin.aiPanel.actions.addProvider')}
    </Button>
  );

  const providerActions = (provider: AIProvider) => (
    <>
      <Button
        type="button"
        size="icon"
        variant="ghost"
        className="h-8 w-8"
        aria-label={iconButtonLabel(t('admin.edit'), provider.displayName)}
        onClick={() => openProviderEditor(provider)}
      >
        <Pencil className="h-3.5 w-3.5" />
      </Button>
      <Button
        type="button"
        size="icon"
        variant="ghost"
        className="h-8 w-8 text-status-failed hover:text-status-failed"
        aria-label={iconButtonLabel(t('admin.aiPanel.actions.disableProvider'), provider.displayName)}
        onClick={() => setDeleteTarget(provider)}
      >
        <Trash2 className="h-3.5 w-3.5" />
      </Button>
    </>
  );

  return (
    <div className="flex flex-1 min-h-0 flex-col">
      <EntityWorkbench<AIProvider>
        tableId="admin.ai.providers"
        title={t('admin.providers')}
        count={providers.length}
        state={providersState}
        rows={providers}
        rowKey={provider => provider.id}
        emptyMessage={t('admin.noProviders')}
        searchPlaceholder={t('admin.aiPanel.filters.providersSearch')}
        toolbar={toolbar}
        rowActions={providerActions}
        matchesFilter={(provider, filter) =>
          matchesFilter([provider.displayName, provider.kind, provider.apiStyle], filter)
        }
        columns={[
          {
            key: 'name',
            header: t('admin.providers'),
            sortValue: provider => provider.displayName,
            cell: provider => (
              <div className="min-w-0">
                <div className="truncate text-sm font-semibold">{provider.displayName}</div>
                <div className="truncate text-[11px] text-muted-foreground">
                  {provider.kind}
                  {provider.apiStyle ? ` · ${provider.apiStyle}` : ''}
                </div>
              </div>
            ),
          },
          {
            key: 'models',
            header: t('admin.aiPanel.metrics.visibleModels'),
            width: 'w-28',
            align: 'right',
            sortValue: provider => provider.modelCount,
            cell: provider => (
              <span className="tabular-nums text-xs text-muted-foreground">{provider.modelCount}</span>
            ),
          },
          {
            key: 'credentials',
            header: t('admin.credentials'),
            width: 'w-28',
            align: 'right',
            sortValue: provider => provider.credentialCount,
            cell: provider => (
              <span className="tabular-nums text-xs text-muted-foreground">{provider.credentialCount}</span>
            ),
          },
          {
            key: 'state',
            header: t('admin.status'),
            width: 'w-32',
            sortValue: provider => provider.lifecycleState,
            cell: provider => {
              const tone = lifecycleTone(provider.lifecycleState);
              const cls = tone === 'ready'
                ? 'text-muted-foreground'
                : tone === 'failed'
                  ? 'text-status-failed'
                  : 'text-status-warning';
              return <span className={`text-xs ${cls}`}>{t(`admin.aiPanel.lifecycleLabels.${provider.lifecycleState}`)}</span>;
            },
          },
        ]}
        renderInspector={provider => ({
          row: provider,
          title: provider.displayName,
          subtitle: provider.kind,
          body: (
            <>
              <div>
                <span className={`status-badge ${badgeClass(lifecycleTone(provider.lifecycleState))}`}>
                  {t(`admin.aiPanel.lifecycleLabels.${provider.lifecycleState}`)}
                </span>
              </div>
              <InspectorSection title={t('admin.aiPanel.fields.apiStyle')}>
                <InspectorField
                  label={t('admin.aiPanel.fields.apiStyle')}
                  value={provider.apiStyle || '—'}
                  mono
                />
                <InspectorField
                  label={t('admin.aiPanel.metrics.visibleModels')}
                  value={provider.modelCount}
                />
                <InspectorField
                  label={t('admin.credentials')}
                  value={provider.credentialCount}
                />
              </InspectorSection>
              <InspectorSection title={t('admin.aiPanel.fields.requirements')}>
                <div className="flex flex-wrap gap-2">
                  <Badge variant="outline">
                    {provider.apiKeyRequired
                      ? t('admin.apiKey')
                      : t('admin.aiPanel.fields.tokenOptional')}
                  </Badge>
                  {provider.baseUrlRequired && (
                    <Badge variant="outline">{t('admin.aiPanel.fields.baseUrl')}</Badge>
                  )}
                  <Badge variant="outline">
                    {t('admin.aiPanel.fields.discovery')}: {provider.modelDiscovery.mode}
                  </Badge>
                </div>
              </InspectorSection>
              {provider.defaultBaseUrl && (
                <InspectorSection title={t('admin.aiPanel.fields.baseUrl')}>
                  <div className="rounded-md border border-border/70 bg-surface-sunken p-3 font-mono text-xs [overflow-wrap:anywhere]">
                    {provider.defaultBaseUrl}
                  </div>
                </InspectorSection>
              )}
              <InspectorSection title={t('admin.aiPanel.fields.identifier')}>
                <InspectorField label={t('admin.aiPanel.fields.identifier')} value={provider.id} mono />
              </InspectorSection>
            </>
          ),
          actions: (
            <div className="grid gap-2 sm:grid-cols-2">
              <Button size="sm" variant="outline" onClick={() => openProviderEditor(provider)}>
                <Pencil className="mr-1.5 h-3.5 w-3.5" /> {t('admin.edit')}
              </Button>
              <Button size="sm" variant="outline" onClick={() => setDeleteTarget(provider)}>
                <Trash2 className="mr-1.5 h-3.5 w-3.5" /> {t('admin.aiPanel.actions.disableProvider')}
              </Button>
            </div>
          ),
        })}
      />

      <Dialog
        open={editorOpen}
        onOpenChange={open => {
          if (!open) resetEditor();
          else setEditorOpen(true);
        }}
      >
        <DialogContent className="max-w-3xl">
          <DialogHeader>
            <DialogTitle>
              {editingProvider
                ? t('admin.aiPanel.dialogs.editProviderTitle')
                : t('admin.aiPanel.dialogs.addProviderTitle')}
            </DialogTitle>
            <DialogDescription>{t('admin.aiPanel.dialogs.providerDescription')}</DialogDescription>
          </DialogHeader>
          <div className="grid gap-4 lg:grid-cols-2">
            <FormInputField
              formState={providerForm.formState}
              id="admin-provider-display-name"
              label={t('admin.aiPanel.fields.displayName')}
              name="displayName"
              registration={providerForm.register('displayName')}
              placeholder={t('admin.aiPanel.placeholders.providerDisplayName')}
            />
            <FormInputField
              formState={providerForm.formState}
              id="admin-provider-kind"
              label={t('admin.aiPanel.fields.providerKind')}
              name="kind"
              registration={providerForm.register('kind')}
              placeholder={t('admin.aiPanel.placeholders.providerKind')}
            />
            <FormSelectField
              control={providerForm.control}
              formState={providerForm.formState}
              id="admin-provider-api-style"
              label={t('admin.aiPanel.fields.apiStyle')}
              name="apiStyle"
              placeholder={t('admin.aiPanel.placeholders.apiStyle')}
            >
              <SelectItem value={PROVIDER_API_STYLE}>{t('admin.aiPanel.apiStyleLabels.openai_compatible')}</SelectItem>
            </FormSelectField>
            <FormSelectField
              control={providerForm.control}
              formState={providerForm.formState}
              id="admin-provider-lifecycle"
              label={t('admin.aiPanel.fields.lifecycle')}
              name="lifecycleState"
              placeholder={t('admin.aiPanel.placeholders.lifecycle')}
            >
              {PROVIDER_LIFECYCLE_STATES.map(state => (
                <SelectItem key={state} value={state}>
                  {t(`admin.aiPanel.lifecycleLabels.${state}`)}
                </SelectItem>
              ))}
            </FormSelectField>
          </div>
          <FormInputField
            formState={providerForm.formState}
            id="admin-provider-base-url"
            label={t('admin.aiPanel.fields.baseUrlOptional')}
            name="defaultBaseUrl"
            registration={providerForm.register('defaultBaseUrl')}
            placeholder={t('admin.aiPanel.placeholders.baseUrl')}
          />
          <FormTextareaField
            formState={providerForm.formState}
            id="admin-provider-profile"
            label={t('admin.aiPanel.fields.providerProfileJson')}
            name="profileJson"
            registration={providerForm.register('profileJson')}
            textareaClassName="min-h-[260px] font-mono text-xs"
          />
          <DialogFooter>
            <Button variant="outline" onClick={resetEditor}>
              {t('admin.cancel')}
            </Button>
            <Button
              disabled={!providerForm.formState.isValid || providerForm.formState.isSubmitting}
              onClick={() => void saveProvider()}
            >
              {providerForm.formState.isSubmitting ? (
                <>
                  <Loader2 className="mr-1.5 h-3.5 w-3.5 animate-spin" /> {t('admin.saving')}
                </>
              ) : (
                <>
                  <Server className="mr-1.5 h-3.5 w-3.5" /> {t('admin.save')}
                </>
              )}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog open={Boolean(deleteTarget)} onOpenChange={open => { if (!open) setDeleteTarget(null); }}>
        <DialogContent className="max-w-md">
          <DialogHeader>
            <DialogTitle>{t('admin.aiPanel.dialogs.disableProviderTitle')}</DialogTitle>
            <DialogDescription>
              {t('admin.aiPanel.dialogs.disableProviderDescription', {
                provider: deleteTarget?.displayName ?? '',
              })}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="outline" onClick={() => setDeleteTarget(null)}>
              {t('admin.cancel')}
            </Button>
            <Button variant="destructive" disabled={deleting} onClick={() => void disableProvider()}>
              {deleting ? (
                <>
                  <Loader2 className="mr-1.5 h-3.5 w-3.5 animate-spin" /> {t('admin.saving')}
                </>
              ) : (
                t('admin.aiPanel.actions.disableProvider')
              )}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
