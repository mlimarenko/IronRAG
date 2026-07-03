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
import { StatusBadge } from '@/shared/components/StatusBadge';
import {
  canEditProviderBaseUrl,
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
import type { AIAccount, AIProvider, AIScopeKind } from '@/shared/types';
import {
  accountStateLabel,
  compareByUpdatedAtDesc,
  localScopeQuery,
  matchesFilter,
  scopeLabel as translatedScopeLabel,
  type AiConfigDataState,
  type AiScopeContext,
} from '@/features/admin/model/aiConfig';

import { EntityWorkbench, InspectorField, InspectorSection } from './EntityWorkbench';

type AccountsSectionProps = {
  selectedScope: AIScopeKind;
  scopeContext: AiScopeContext;
  providers: AIProvider[];
  accountsState: AiConfigDataState<AIAccount[]>;
  invalidateAll: () => void;
  openAddRequest?: number;
};

export function AccountsSection({
  selectedScope,
  scopeContext,
  providers,
  accountsState,
  invalidateAll,
  openAddRequest = 0,
}: AccountsSectionProps) {
  const { t } = useTranslation();
  const [accountOpen, setAccountOpen] = useState(false);
  const [editingAccount, setEditingAccount] = useState<AIAccount | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<AIAccount | null>(null);
  const [deletingAccount, setDeletingAccount] = useState(false);
  const [accountLabelTouched, setAccountLabelTouched] = useState(false);
  const accountSchema = useMemo(
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
        const baseUrlEditable = canEditProviderBaseUrl(provider);
        if (policy.baseUrlRequired && baseUrlEditable && !values.baseUrl.trim()) {
          context.addIssue({
            code: 'custom',
            message: t('admin.aiPanel.fields.baseUrlRequiredHint'),
            path: ['baseUrl'],
          });
        }
        if (
          policy.baseUrlRequired
          && !baseUrlEditable
          && !normalizeProviderBaseUrl(provider, provider.defaultBaseUrl)
        ) {
          context.addIssue({
            code: 'custom',
            message: t('admin.aiPanel.fields.baseUrlRequiredHint'),
            path: ['baseUrl'],
          });
        }
        if (!editingAccount && policy.apiKeyRequired && !values.apiKey.trim()) {
          context.addIssue({
            code: 'custom',
            message: t('admin.aiPanel.fields.tokenRequiredHint'),
            path: ['apiKey'],
          });
        }
      }),
    [editingAccount, providers, t],
  );
  const accountForm = useTypedForm({
    schema: accountSchema,
    defaultValues: {
      apiKey: '',
      baseUrl: '',
      label: '',
      providerId: '',
    },
    mode: 'onChange',
  });
  const accountProviderId = accountForm.watch('providerId');
  const accountBaseUrl = accountForm.watch('baseUrl');
  const accountApiKey = accountForm.watch('apiKey');
  const {
    reset: resetAccountForm,
    setValue: setAccountValue,
  } = accountForm;

  const selectedProvider = providers.find(entry => entry.id === accountProviderId) ?? null;
  const scopeLabel = (value: AIScopeKind) => translatedScopeLabel(value, t);
  const defaultAccountLabel = useCallback(
    (provider: AIProvider | null | undefined) =>
      provider ? t('admin.aiPanel.placeholders.accountLabelForProvider', { provider: provider.displayName }) : '',
    [t],
  );
  const accounts = useMemo(() => accountsState.data ?? [], [accountsState.data]);
  const sortedAccounts = useMemo(
    () => accounts.slice().sort(compareByUpdatedAtDesc),
    [accounts],
  );
  const lastOpenAddRequestRef = useRef(0);

  const resetAccountDialog = () => {
    setAccountOpen(false);
    setEditingAccount(null);
    setAccountLabelTouched(false);
    resetAccountForm({
      apiKey: '',
      baseUrl: '',
      label: '',
      providerId: '',
    });
  };

  const openNewAccountEditor = useCallback(() => {
    const configuredProviderIds = new Set(accounts.map(entry => entry.providerId));
    const provider =
      providers.find(entry => entry.lifecycleState === 'active' && !configuredProviderIds.has(entry.id)) ??
      providers.find(entry => entry.lifecycleState === 'active') ??
      providers[0];
    setEditingAccount(null);
    setAccountLabelTouched(false);
    resetAccountForm({
      apiKey: '',
      baseUrl: provider ? normalizeProviderBaseUrl(provider, provider.defaultBaseUrl) : '',
      label: defaultAccountLabel(provider),
      providerId: provider?.id ?? '',
    });
    setAccountOpen(true);
  }, [accounts, defaultAccountLabel, providers, resetAccountForm]);

  useEffect(() => {
    if (openAddRequest <= 0 || lastOpenAddRequestRef.current === openAddRequest) {
      return;
    }
    lastOpenAddRequestRef.current = openAddRequest;
    openNewAccountEditor();
  }, [openAddRequest, openNewAccountEditor]);

  const accountSaveErrorMessage = (saveError: unknown) => {
    const message = String((saveError as { message?: string } | null)?.message ?? '');
    if (message.includes('provider credential validation failed')) {
      return t('admin.aiPanel.messages.accountValidationFailed');
    }
    return t('admin.aiPanel.messages.accountSaveFailed');
  };

  const openAccountEditor = (entry: AIAccount) => {
    const provider = providers.find(providerEntry => providerEntry.id === entry.providerId);
    setEditingAccount(entry);
    setAccountLabelTouched(true);
    resetAccountForm({
      apiKey: '',
      baseUrl: provider ? normalizeProviderBaseUrl(provider, entry.baseUrl ?? provider.defaultBaseUrl) : entry.baseUrl ?? '',
      label: entry.label,
      providerId: entry.providerId,
    });
    setAccountOpen(true);
  };

  const handleAccountProviderChange = (providerId: string) => {
    const provider = providers.find(entry => entry.id === providerId) ?? null;
    setAccountValue('baseUrl', provider ? normalizeProviderBaseUrl(provider, provider.defaultBaseUrl) : '', {
      shouldDirty: true,
      shouldValidate: true,
    });
    setAccountValue('apiKey', '', {
      shouldDirty: true,
      shouldValidate: true,
    });
    if (!editingAccount && !accountLabelTouched) {
      setAccountValue('label', defaultAccountLabel(provider), {
        shouldDirty: true,
        shouldValidate: true,
      });
    }
  };

  const saveAccount = accountForm.submitWithMutation(
    {
      mutateAsync: async (values) => {
        const provider = providers.find(entry => entry.id === values.providerId) ?? null;
        if (!provider) {
          throw new Error(t('admin.provider'));
        }
        const baseUrlEditable = canEditProviderBaseUrl(provider);
        if (editingAccount) {
          await adminApi.updateAccount(editingAccount.id, {
            label: values.label.trim(),
            apiKey: values.apiKey.trim() || undefined,
            baseUrl: baseUrlEditable ? values.baseUrl.trim() || undefined : undefined,
            credentialState: 'active',
          });
        } else {
          const localParams = localScopeQuery(selectedScope, scopeContext);
          await adminApi.createAccount({
            ...(localParams.query ?? {}),
            providerCatalogId: values.providerId,
            label: values.label.trim(),
            apiKey: values.apiKey.trim() || undefined,
            baseUrl: baseUrlEditable ? values.baseUrl.trim() || undefined : undefined,
          });
        }
        resetAccountDialog();
        invalidateAll();
        toast.success(t('admin.aiPanel.messages.accountSaved'));
      },
    },
    {
      errorMessage: accountSaveErrorMessage,
    },
  );

  const deleteAccount = async () => {
    if (!deleteTarget || deletingAccount) return;
    setDeletingAccount(true);
    try {
      await adminApi.deleteAccount(deleteTarget.id);
      setDeleteTarget(null);
      invalidateAll();
      toast.success(t('admin.aiPanel.messages.accountDeleted'));
    } catch (err) {
      toast.error(errorMessage(err, t('admin.aiPanel.messages.accountDeleteFailed')));
    } finally {
      setDeletingAccount(false);
    }
  };

  const toolbar = (
    <Button size="sm" onClick={openNewAccountEditor}>
      <KeyRound className="mr-1.5 h-3.5 w-3.5" /> {t('admin.add')}
    </Button>
  );

  const accountApiKeyError = fieldErrorMessage(accountForm.formState.errors, 'apiKey');
  const accountBaseUrlError = fieldErrorMessage(accountForm.formState.errors, 'baseUrl');

  return (
    <div className="flex flex-1 min-h-0 flex-col">
      <EntityWorkbench<AIAccount>
        tableId="admin.ai.accounts"
        title={t('admin.aiPanel.accountsTitle')}
        count={sortedAccounts.length}
        state={accountsState}
        rows={sortedAccounts}
        rowKey={entry => entry.id}
        emptyMessage={t('admin.aiPanel.empty.noLocalAccounts')}
        searchPlaceholder={t('admin.aiPanel.filters.accountsSearch')}
        toolbar={toolbar}
        rowActions={entry => (
          <>
            <Button
              type="button"
              size="icon"
              variant="ghost"
              className="h-8 w-8"
              aria-label={`${t('admin.edit')}: ${entry.label}`}
              onClick={() => openAccountEditor(entry)}
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
              accountStateLabel(entry.state, t),
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
                <div className="truncate text-2xs text-muted-foreground">
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
              return <span className={`text-xs ${cls}`}>{accountStateLabel(entry.state, t)}</span>;
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
                  <StatusBadge tone={entry.state === 'active' ? 'ready' : entry.state === 'invalid' ? 'failed' : 'warning'}>
                    {accountStateLabel(entry.state, t)}
                  </StatusBadge>
                </div>
                <InspectorSection title={t('admin.aiPanel.fields.scope')}>
                  <Badge variant="outline">{scopeLabel(entry.scopeKind)}</Badge>
                </InspectorSection>
                <InspectorSection title={t('admin.apiKey')}>
                  <div className="rounded-md bg-surface-sunken p-3 font-mono text-xs [overflow-wrap:anywhere]">
                    {entry.apiKeySummary || t('admin.aiPanel.tokenOptional')}
                  </div>
                </InspectorSection>
                {baseUrl && (
                  <InspectorSection title={t('admin.aiPanel.fields.baseUrl')}>
                    <div className="rounded-md bg-surface-sunken p-3 font-mono text-xs [overflow-wrap:anywhere]">
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
                  onClick={() => openAccountEditor(entry)}
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

      <Dialog open={accountOpen} onOpenChange={open => { if (!open) resetAccountDialog(); else setAccountOpen(true); }}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{editingAccount ? t('admin.aiPanel.dialogs.editAccountTitle') : t('admin.aiPanel.dialogs.addAccountTitle')}</DialogTitle>
            <DialogDescription>{t('admin.aiPanel.dialogs.accountDescription')}</DialogDescription>
          </DialogHeader>
          <div className="space-y-4">
            <FormSelectField
              control={accountForm.control}
              disabled={Boolean(editingAccount)}
              formState={accountForm.formState}
              id="admin-account-provider"
              label={t('admin.provider')}
              name="providerId"
              onValueChange={handleAccountProviderChange}
              placeholder={t('admin.aiPanel.placeholders.selectProvider')}
            >
              {providers.map(entry => (
                <SelectItem key={entry.id} value={entry.id}>{entry.displayName}</SelectItem>
              ))}
            </FormSelectField>
            <FormInputField
              formState={accountForm.formState}
              id="admin-account-label"
              label={t('admin.label')}
              name="label"
              onValueChange={() => setAccountLabelTouched(true)}
              registration={accountForm.register('label')}
              placeholder={t('admin.aiPanel.placeholders.accountLabel')}
            />
            <ProviderCredentialFields
              provider={selectedProvider}
              idPrefix="admin-provider-credential"
              apiKey={accountApiKey}
              baseUrl={accountBaseUrl}
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
              {...(accountApiKeyError !== undefined ? { apiKeyError: accountApiKeyError } : {})}
              {...(accountBaseUrlError !== undefined ? { baseUrlError: accountBaseUrlError } : {})}
              onApiKeyChange={value => setAccountValue('apiKey', value, { shouldDirty: true, shouldValidate: true })}
              onBaseUrlChange={value => setAccountValue('baseUrl', value, { shouldDirty: true, shouldValidate: true })}
              preserveExistingSecret={Boolean(editingAccount)}
            />
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={resetAccountDialog}>{t('admin.cancel')}</Button>
            <Button disabled={!accountForm.formState.isValid || accountForm.formState.isSubmitting} onClick={() => void saveAccount()}>
              {accountForm.formState.isSubmitting ? <><Loader2 className="mr-1.5 h-3.5 w-3.5 animate-spin" /> {t('admin.saving')}</> : t('admin.save')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog open={Boolean(deleteTarget)} onOpenChange={open => { if (!open) setDeleteTarget(null); }}>
        <DialogContent className="max-w-md">
          <DialogHeader>
            <DialogTitle>{t('admin.aiPanel.dialogs.deleteAccountTitle')}</DialogTitle>
            <DialogDescription>
              {t('admin.aiPanel.dialogs.deleteAccountDescription', {
                account: deleteTarget?.label ?? '',
              })}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="outline" onClick={() => setDeleteTarget(null)}>{t('admin.cancel')}</Button>
            <Button variant="destructive" disabled={deletingAccount} onClick={() => void deleteAccount()}>
              {deletingAccount ? <><Loader2 className="mr-1.5 h-3.5 w-3.5 animate-spin" /> {t('admin.saving')}</> : t('admin.delete')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
