import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { KeyRound, Loader2, Pencil, Trash2 } from 'lucide-react';
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
import {
  normalizeProviderBaseUrl,
  resolveProviderCredentialPolicy,
} from '@/shared/lib/ai-provider';
import { errorMessage } from '@/shared/lib/errorMessage';
import { ProviderCredentialFields } from '@/shared/components/ai-provider/ProviderCredentialFields';
import {
  fieldErrorMessage,
  FormInputField,
  FormSelectField,
  nonEmptyString,
  useTypedForm,
} from '@/shared/forms';
import type { AICredential, AIProvider, AIScopeKind } from '@/shared/types';
import {
  badgeClass,
  compareByUpdatedAtDesc,
  credentialStateLabel,
  localScopeQuery,
  matchesFilter,
  scopeLabel as translatedScopeLabel,
  type AiConfigDataState,
  type AiScopeContext,
} from '@/features/admin/model/aiConfig';

import { EntityWorkbench, InspectorField, InspectorSection } from './EntityWorkbench';

function canOverrideBaseUrl(
  provider: { credentialSource?: string } | null | undefined,
): boolean {
  return Boolean(provider) && provider?.credentialSource !== 'env';
}

type CredentialsSectionProps = {
  selectedScope: AIScopeKind;
  scopeContext: AiScopeContext;
  providers: AIProvider[];
  credentialsState: AiConfigDataState<AICredential[]>;
  invalidateAll: () => void;
  openAddRequest?: number;
};

export function CredentialsSection({
  selectedScope,
  scopeContext,
  providers,
  credentialsState,
  invalidateAll,
  openAddRequest = 0,
}: CredentialsSectionProps) {
  const { t } = useTranslation();
  const [credentialOpen, setCredentialOpen] = useState(false);
  const [editingCredential, setEditingCredential] = useState<AICredential | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<AICredential | null>(null);
  const [deletingCredential, setDeletingCredential] = useState(false);
  const [credentialLabelTouched, setCredentialLabelTouched] = useState(false);
  const credentialSchema = useMemo(
    () =>
      z.object({
        apiKey: z.string(),
        baseUrl: z.string(),
        label: nonEmptyString(t('admin.label')),
        providerId: nonEmptyString(t('admin.provider')),
      }).superRefine((values, context) => {
        const provider = providers.find(entry => entry.id === values.providerId) ?? null;
        if (!provider) {
          context.addIssue({
            code: 'custom',
            message: t('admin.provider'),
            path: ['providerId'],
          });
          return;
        }
        const policy = resolveProviderCredentialPolicy(provider);
        if (policy.baseUrlRequired && canOverrideBaseUrl(provider) && !values.baseUrl.trim()) {
          context.addIssue({
            code: 'custom',
            message: t('admin.aiPanel.fields.baseUrlRequiredHint'),
            path: ['baseUrl'],
          });
        }
        if (
          provider.credentialSource !== 'env'
          && policy.baseUrlRequired
          && !canOverrideBaseUrl(provider)
          && !normalizeProviderBaseUrl(provider, provider.defaultBaseUrl)
        ) {
          context.addIssue({
            code: 'custom',
            message: t('admin.aiPanel.fields.baseUrlRequiredHint'),
            path: ['baseUrl'],
          });
        }
        if (!editingCredential && policy.apiKeyRequired && !values.apiKey.trim()) {
          context.addIssue({
            code: 'custom',
            message: t('admin.aiPanel.fields.tokenRequiredHint'),
            path: ['apiKey'],
          });
        }
      }),
    [editingCredential, providers, t],
  );
  const credentialForm = useTypedForm({
    schema: credentialSchema,
    defaultValues: {
      apiKey: '',
      baseUrl: '',
      label: '',
      providerId: '',
    },
    mode: 'onChange',
  });
  const credentialProviderId = credentialForm.watch('providerId');
  const credentialBaseUrl = credentialForm.watch('baseUrl');
  const credentialApiKey = credentialForm.watch('apiKey');
  const {
    reset: resetCredentialForm,
    setValue: setCredentialValue,
  } = credentialForm;

  const selectedProvider = providers.find(entry => entry.id === credentialProviderId) ?? null;
  const scopeLabel = (value: AIScopeKind) => translatedScopeLabel(value, t);
  const defaultCredentialLabel = useCallback(
    (provider: AIProvider | null | undefined) =>
      provider ? t('admin.aiPanel.placeholders.credentialLabelForProvider', { provider: provider.displayName }) : '',
    [t],
  );
  const credentials = useMemo(() => credentialsState.data ?? [], [credentialsState.data]);
  const sortedCredentials = useMemo(
    () => credentials.slice().sort(compareByUpdatedAtDesc),
    [credentials],
  );
  const lastOpenAddRequestRef = useRef(0);

  const resetCredentialDialog = () => {
    setCredentialOpen(false);
    setEditingCredential(null);
    setCredentialLabelTouched(false);
    resetCredentialForm({
      apiKey: '',
      baseUrl: '',
      label: '',
      providerId: '',
    });
  };

  const openNewCredentialEditor = useCallback(() => {
    const configuredProviderIds = new Set(credentials.map(entry => entry.providerId));
    const provider =
      providers.find(entry => entry.lifecycleState === 'active' && !configuredProviderIds.has(entry.id)) ??
      providers.find(entry => entry.lifecycleState === 'active') ??
      providers[0];
    setEditingCredential(null);
    setCredentialLabelTouched(false);
    resetCredentialForm({
      apiKey: '',
      baseUrl: provider ? normalizeProviderBaseUrl(provider, provider.defaultBaseUrl) : '',
      label: defaultCredentialLabel(provider),
      providerId: provider?.id ?? '',
    });
    setCredentialOpen(true);
  }, [credentials, defaultCredentialLabel, providers, resetCredentialForm]);

  useEffect(() => {
    if (openAddRequest <= 0 || lastOpenAddRequestRef.current === openAddRequest) {
      return;
    }
    lastOpenAddRequestRef.current = openAddRequest;
    openNewCredentialEditor();
  }, [openAddRequest, openNewCredentialEditor]);

  const credentialSaveErrorMessage = (saveError: unknown) => {
    const message = String((saveError as { message?: string } | null)?.message ?? '');
    if (message.includes('provider credential validation failed')) {
      return t('admin.aiPanel.messages.credentialValidationFailed');
    }
    return t('admin.aiPanel.messages.credentialSaveFailed');
  };

  const openCredentialEditor = (entry: AICredential) => {
    const provider = providers.find(providerEntry => providerEntry.id === entry.providerId);
    setEditingCredential(entry);
    setCredentialLabelTouched(true);
    resetCredentialForm({
      apiKey: '',
      baseUrl: provider ? normalizeProviderBaseUrl(provider, entry.baseUrl ?? provider.defaultBaseUrl) : entry.baseUrl ?? '',
      label: entry.label,
      providerId: entry.providerId,
    });
    setCredentialOpen(true);
  };

  const handleCredentialProviderChange = (providerId: string) => {
    const provider = providers.find(entry => entry.id === providerId) ?? null;
    setCredentialValue('baseUrl', provider ? normalizeProviderBaseUrl(provider, provider.defaultBaseUrl) : '', {
      shouldDirty: true,
      shouldValidate: true,
    });
    setCredentialValue('apiKey', '', {
      shouldDirty: true,
      shouldValidate: true,
    });
    if (!editingCredential && !credentialLabelTouched) {
      setCredentialValue('label', defaultCredentialLabel(provider), {
        shouldDirty: true,
        shouldValidate: true,
      });
    }
  };

  const saveCredential = credentialForm.submitWithMutation(
    {
      mutateAsync: async (values) => {
        const provider = providers.find(entry => entry.id === values.providerId) ?? null;
        if (!provider) {
          throw new Error(t('admin.provider'));
        }
        if (editingCredential) {
          await adminApi.updateCredential(editingCredential.id, {
            label: values.label.trim(),
            apiKey: values.apiKey.trim() || undefined,
            baseUrl: canOverrideBaseUrl(provider) ? values.baseUrl.trim() || undefined : undefined,
            credentialState: 'active',
          });
        } else {
          const localParams = localScopeQuery(selectedScope, scopeContext);
          await adminApi.createCredential({
            ...(localParams.query ?? {}),
            providerCatalogId: values.providerId,
            label: values.label.trim(),
            apiKey: values.apiKey.trim() || undefined,
            baseUrl: canOverrideBaseUrl(provider) ? values.baseUrl.trim() || undefined : undefined,
          });
        }
        resetCredentialDialog();
        invalidateAll();
        toast.success(t('admin.aiPanel.messages.credentialSaved'));
      },
    },
    {
      errorMessage: credentialSaveErrorMessage,
    },
  );

  const deleteCredential = async () => {
    if (!deleteTarget || deletingCredential) return;
    setDeletingCredential(true);
    try {
      await adminApi.deleteCredential(deleteTarget.id);
      setDeleteTarget(null);
      invalidateAll();
      toast.success(t('admin.aiPanel.messages.credentialDeleted'));
    } catch (err) {
      toast.error(errorMessage(err, t('admin.aiPanel.messages.credentialDeleteFailed')));
    } finally {
      setDeletingCredential(false);
    }
  };

  const toolbar = (
    <Button size="sm" onClick={openNewCredentialEditor}>
      <KeyRound className="mr-1.5 h-3.5 w-3.5" /> {t('admin.add')}
    </Button>
  );

  return (
    <div className="flex flex-1 min-h-0 flex-col">
      <EntityWorkbench<AICredential>
        tableId="admin.ai.credentials"
        title={t('admin.aiPanel.credentialsTitle')}
        count={sortedCredentials.length}
        state={credentialsState}
        rows={sortedCredentials}
        rowKey={entry => entry.id}
        emptyMessage={t('admin.aiPanel.empty.noLocalCredentials')}
        searchPlaceholder={t('admin.aiPanel.filters.credentialsSearch')}
        toolbar={toolbar}
        rowActions={entry => (
          <>
            <Button
              type="button"
              size="icon"
              variant="ghost"
              className="h-8 w-8"
              aria-label={`${t('admin.edit')}: ${entry.label}`}
              onClick={() => openCredentialEditor(entry)}
            >
              <Pencil className="h-3.5 w-3.5" />
            </Button>
            <Button
              type="button"
              size="icon"
              variant="ghost"
              className="h-8 w-8 text-status-failed hover:text-status-failed"
              aria-label={`${t('admin.delete')}: ${entry.label}`}
              onClick={() => setDeleteTarget(entry)}
            >
              <Trash2 className="h-3.5 w-3.5" />
            </Button>
          </>
        )}
        matchesFilter={(entry, filter) =>
          matchesFilter(
            [
              entry.label,
              entry.providerName,
              entry.providerKind,
              entry.baseUrl,
              entry.apiKeySummary,
              credentialStateLabel(entry.state, t),
            ],
            filter,
          )
        }
        columns={[
          {
            key: 'label',
            header: t('admin.label'),
            sortValue: entry => entry.label,
            cell: entry => (
              <div className="min-w-0">
                <div className="truncate text-sm font-semibold" title={entry.label}>
                  {entry.label}
                </div>
                <div className="truncate text-[11px] text-muted-foreground">
                  {entry.providerName}
                </div>
              </div>
            ),
          },
          {
            key: 'token',
            header: t('admin.apiKey'),
            width: 'w-44',
            sortValue: entry => entry.apiKeySummary,
            cell: entry => (
              <span className="truncate font-mono text-xs text-muted-foreground" title={entry.apiKeySummary}>
                {entry.apiKeySummary || t('admin.aiPanel.tokenOptional')}
              </span>
            ),
          },
          {
            key: 'scope',
            header: t('admin.aiPanel.fields.scope'),
            width: 'w-32',
            sortValue: entry => entry.scopeKind,
            cell: entry => (
              <span className="text-xs text-muted-foreground">{scopeLabel(entry.scopeKind)}</span>
            ),
          },
          {
            key: 'state',
            header: t('admin.status'),
            width: 'w-28',
            sortValue: entry => entry.state,
            cell: entry => {
              const tone = entry.state === 'active' ? 'ready' : entry.state === 'invalid' ? 'failed' : 'warning';
              const cls = tone === 'ready'
                ? 'text-muted-foreground'
                : tone === 'failed'
                  ? 'text-status-failed'
                  : 'text-status-warning';
              return <span className={`text-xs ${cls}`}>{credentialStateLabel(entry.state, t)}</span>;
            },
          },
        ]}
        renderInspector={entry => {
          const provider = providers.find(p => p.id === entry.providerId);
          const baseUrl = entry.baseUrl && entry.provider
            ? normalizeProviderBaseUrl(entry.provider, entry.baseUrl)
            : entry.baseUrl || provider?.defaultBaseUrl || '';
          return {
            row: entry,
            title: entry.label,
            subtitle: entry.providerName,
            body: (
              <>
                <div>
                  <span className={`status-badge ${badgeClass(entry.state === 'active' ? 'ready' : entry.state === 'invalid' ? 'failed' : 'warning')}`}>
                    {credentialStateLabel(entry.state, t)}
                  </span>
                </div>
                <InspectorSection title={t('admin.aiPanel.fields.scope')}>
                  <Badge variant="outline">{scopeLabel(entry.scopeKind)}</Badge>
                </InspectorSection>
                <InspectorSection title={t('admin.apiKey')}>
                  <div className="rounded-md border border-border/70 bg-surface-sunken p-3 font-mono text-xs [overflow-wrap:anywhere]">
                    {entry.apiKeySummary || t('admin.aiPanel.tokenOptional')}
                  </div>
                </InspectorSection>
                {baseUrl && (
                  <InspectorSection title={t('admin.aiPanel.fields.baseUrl')}>
                    <div className="rounded-md border border-border/70 bg-surface-sunken p-3 font-mono text-xs [overflow-wrap:anywhere]">
                      {baseUrl}
                    </div>
                  </InspectorSection>
                )}
                <InspectorSection title={t('admin.aiPanel.fields.identifier')}>
                  <InspectorField label={t('admin.aiPanel.fields.identifier')} value={entry.id} mono />
                </InspectorSection>
              </>
            ),
            actions: (
              <div className="grid gap-2 sm:grid-cols-2">
                <Button
                  size="sm"
                  variant="outline"
                  onClick={() => openCredentialEditor(entry)}
                >
                  <Pencil className="mr-1.5 h-3.5 w-3.5" /> {t('admin.edit')}
                </Button>
                <Button
                  size="sm"
                  variant="outline"
                  onClick={() => setDeleteTarget(entry)}
                >
                  <Trash2 className="mr-1.5 h-3.5 w-3.5" /> {t('admin.delete')}
                </Button>
              </div>
            ),
          };
        }}
      />

      <Dialog open={credentialOpen} onOpenChange={open => { if (!open) resetCredentialDialog(); else setCredentialOpen(true); }}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{editingCredential ? t('admin.aiPanel.dialogs.editCredentialTitle') : t('admin.aiPanel.dialogs.addCredentialTitle')}</DialogTitle>
            <DialogDescription>{t('admin.aiPanel.dialogs.credentialDescription')}</DialogDescription>
          </DialogHeader>
          <div className="space-y-4">
            <FormSelectField
              control={credentialForm.control}
              disabled={Boolean(editingCredential)}
              formState={credentialForm.formState}
              id="admin-credential-provider"
              label={t('admin.provider')}
              name="providerId"
              onValueChange={handleCredentialProviderChange}
              placeholder={t('admin.aiPanel.placeholders.selectProvider')}
            >
              {providers.map(entry => (
                <SelectItem key={entry.id} value={entry.id}>{entry.displayName}</SelectItem>
              ))}
            </FormSelectField>
            <FormInputField
              formState={credentialForm.formState}
              id="admin-credential-label"
              label={t('admin.label')}
              name="label"
              onValueChange={() => setCredentialLabelTouched(true)}
              registration={credentialForm.register('label')}
              placeholder={t('admin.aiPanel.placeholders.credentialLabel')}
            />
            <ProviderCredentialFields
              provider={selectedProvider}
              idPrefix="admin-provider-credential"
              apiKey={credentialApiKey}
              baseUrl={credentialBaseUrl}
              allowBaseUrlOverride
              labels={{
                apiKeyRequired: t('admin.apiKey'),
                apiKeyOptional: t('admin.aiPanel.fields.tokenOptional'),
                apiKeyPlaceholder: t('admin.aiPanel.placeholders.providerToken'),
                apiKeyRequiredHint: t('admin.aiPanel.fields.tokenRequiredHint'),
                baseUrlRequired: t('admin.aiPanel.fields.baseUrl'),
                baseUrlOptional: t('admin.aiPanel.fields.baseUrlOptional'),
                baseUrlRequiredHint: t('admin.aiPanel.fields.baseUrlRequiredHint'),
                fixedBaseUrlHint: t('admin.aiPanel.fields.fixedBaseUrlHint'),
                keepSecretPlaceholder: t('admin.aiPanel.placeholders.keepSecret'),
              }}
              apiKeyError={fieldErrorMessage(credentialForm.formState.errors, 'apiKey')}
              baseUrlError={fieldErrorMessage(credentialForm.formState.errors, 'baseUrl')}
              onApiKeyChange={value => setCredentialValue('apiKey', value, { shouldDirty: true, shouldValidate: true })}
              onBaseUrlChange={value => setCredentialValue('baseUrl', value, { shouldDirty: true, shouldValidate: true })}
              preserveExistingSecret={Boolean(editingCredential)}
            />
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={resetCredentialDialog}>{t('admin.cancel')}</Button>
            <Button disabled={!credentialForm.formState.isValid || credentialForm.formState.isSubmitting} onClick={() => void saveCredential()}>
              {credentialForm.formState.isSubmitting ? <><Loader2 className="mr-1.5 h-3.5 w-3.5 animate-spin" /> {t('admin.saving')}</> : t('admin.save')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog open={Boolean(deleteTarget)} onOpenChange={open => { if (!open) setDeleteTarget(null); }}>
        <DialogContent className="max-w-md">
          <DialogHeader>
            <DialogTitle>{t('admin.aiPanel.dialogs.deleteCredentialTitle')}</DialogTitle>
            <DialogDescription>
              {t('admin.aiPanel.dialogs.deleteCredentialDescription', {
                credential: deleteTarget?.label ?? '',
              })}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="outline" onClick={() => setDeleteTarget(null)}>{t('admin.cancel')}</Button>
            <Button variant="destructive" disabled={deletingCredential} onClick={() => void deleteCredential()}>
              {deletingCredential ? <><Loader2 className="mr-1.5 h-3.5 w-3.5 animate-spin" /> {t('admin.saving')}</> : t('admin.delete')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
