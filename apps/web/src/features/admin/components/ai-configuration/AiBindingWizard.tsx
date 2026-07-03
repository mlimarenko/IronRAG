import { useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { ArrowLeft, ArrowRight, KeyRound, Loader2, Sparkles } from 'lucide-react';
import { toast } from 'sonner';
import { z } from 'zod';

import { adminApi } from '@/shared/api';
import { Button } from '@/shared/components/ui/button';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/shared/components/ui/dialog';
import { Input } from '@/shared/components/ui/input';
import { Label } from '@/shared/components/ui/label';
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/shared/components/ui/select';
import { ProviderCredentialFields } from '@/shared/components/ai-provider/ProviderCredentialFields';
import { canEditProviderBaseUrl, normalizeProviderBaseUrl } from '@/shared/lib/ai-provider';
import { errorMessage } from '@/shared/lib/errorMessage';
import {
  FormInputField,
  FormSelectField,
  FormTextareaField,
  useTypedForm,
} from '@/shared/forms';
import type { AIAccount, AIBindingAssignment, AIModelOption, AIProvider, AIPurpose, AIScopeKind, PricingRule } from '@/shared/types';
import {
  bindingParamsRequestBody,
  bindingParamsSchema,
  compactScopeQuery,
  formatModelPriceSuffix,
  localScopeQuery,
  purposeLabel,
  REQUIRED_RUNTIME_PURPOSE_ORDER,
  resolveBindingForPurpose,
  resolveModelPriceSummary,
  type AiScopeContext,
} from '@/features/admin/model/aiConfig';

type AccountMode = 'existing' | 'new';
type WizardStep = 'account' | 'model';

type AiBindingWizardProps = {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  selectedScope: AIScopeKind;
  scopeContext: AiScopeContext;
  activeWorkspaceName?: string | undefined;
  activeLibraryName?: string | undefined;
  onScopeChange: (scope: AIScopeKind) => void;
  availableAccounts: AIAccount[];
  providers: AIProvider[];
  models: AIModelOption[];
  prices: PricingRule[];
  bindingsForScope: AIBindingAssignment[];
  instanceBindings: AIBindingAssignment[];
  workspaceBindings: AIBindingAssignment[];
  invalidateAll: () => void;
};

/**
 * Guided binding wizard (ADM-01, variant A). Two steps for a chosen purpose +
 * scope: (1) an AI account — reuse an existing one or create a new
 * provider+key account inline; (2) a model plus optional advanced
 * parameters. Unlike the pre-simplification wizard this one creates the
 * binding directly rather than routing to other sections — there is no
 * separate "preset" concept left to assemble.
 */
export function AiBindingWizard({
  open,
  onOpenChange,
  selectedScope,
  scopeContext,
  activeWorkspaceName,
  activeLibraryName,
  onScopeChange,
  availableAccounts,
  providers,
  models,
  prices,
  bindingsForScope,
  instanceBindings,
  workspaceBindings,
  invalidateAll,
}: AiBindingWizardProps) {
  const { t } = useTranslation();
  const topSchema = useMemo(() => z.object({ purpose: z.string(), scope: z.string() }), []);
  const topForm = useTypedForm({
    schema: topSchema,
    defaultValues: { purpose: REQUIRED_RUNTIME_PURPOSE_ORDER[0] ?? '', scope: selectedScope },
  });
  const { setValue: setTopValue } = topForm;
  const purpose = topForm.watch('purpose') as AIPurpose;

  useEffect(() => {
    setTopValue('scope', selectedScope);
  }, [selectedScope, setTopValue]);

  const scopeOptions = useMemo(
    () =>
      [
        { kind: 'instance' as const, label: t('admin.aiPanel.scopeCards.instanceTitle'), enabled: true },
        {
          kind: 'workspace' as const,
          label: activeWorkspaceName ?? t('admin.aiPanel.scopeCards.workspaceTitle'),
          enabled: Boolean(activeWorkspaceName),
        },
        {
          kind: 'library' as const,
          label: activeLibraryName ?? t('admin.aiPanel.scopeCards.libraryTitle'),
          enabled: Boolean(activeLibraryName),
        },
      ].filter((option) => option.enabled),
    [activeWorkspaceName, activeLibraryName, t],
  );

  const [step, setStep] = useState<WizardStep>('account');
  const [accountMode, setAccountMode] = useState<AccountMode>('existing');
  const [selectedAccountId, setSelectedAccountId] = useState('');
  const [newAccountProviderId, setNewAccountProviderId] = useState('');
  const [newAccountLabel, setNewAccountLabel] = useState('');
  const [newAccountApiKey, setNewAccountApiKey] = useState('');
  const [newAccountBaseUrl, setNewAccountBaseUrl] = useState('');
  const [accountBusy, setAccountBusy] = useState(false);
  const [accountError, setAccountError] = useState('');
  const [bindingBusy, setBindingBusy] = useState(false);

  const activeAccounts = useMemo(
    () => availableAccounts.filter(entry => entry.state === 'active'),
    [availableAccounts],
  );
  const newAccountProvider = providers.find(entry => entry.id === newAccountProviderId) ?? null;

  const bindingSchema = useMemo(() => bindingParamsSchema(t), [t]);
  const bindingForm = useTypedForm({
    schema: bindingSchema,
    defaultValues: {
      accountId: '',
      modelCatalogId: '',
      systemPrompt: '',
      temperature: '',
      topP: '',
      maxOutputTokens: '',
      extraParametersJson: '',
    },
    mode: 'onChange',
  });

  // Reset the wizard's local state when the dialog transitions from closed to
  // open. Adjusted during render (React's documented pattern for syncing
  // state to a changing prop) instead of an effect, so it can't cascade an
  // extra render pass the way `setState` inside `useEffect` would.
  const [wasOpen, setWasOpen] = useState(open);
  if (open !== wasOpen) {
    setWasOpen(open);
    if (open) {
      setStep('account');
      setAccountMode(activeAccounts.length > 0 ? 'existing' : 'new');
      setSelectedAccountId(activeAccounts[0]?.id ?? '');
      setNewAccountProviderId(providers.find(entry => entry.lifecycleState === 'active')?.id ?? providers[0]?.id ?? '');
      setNewAccountLabel('');
      setNewAccountApiKey('');
      setNewAccountBaseUrl('');
      setAccountError('');
      bindingForm.reset({
        accountId: '',
        modelCatalogId: '',
        systemPrompt: '',
        temperature: '',
        topP: '',
        maxOutputTokens: '',
        extraParametersJson: '',
      });
    }
  }

  const accountProviderId = accountMode === 'existing'
    ? activeAccounts.find(entry => entry.id === selectedAccountId)?.providerId
    : newAccountProviderId;
  const modelOptions = models.filter(
    entry => entry.allowedBindingPurposes.includes(purpose) && (!accountProviderId || entry.providerCatalogId === accountProviderId),
  );

  const canAdvanceFromAccount = accountMode === 'existing'
    ? Boolean(selectedAccountId)
    : Boolean(newAccountProviderId && newAccountLabel.trim());

  const goToModelStep = async () => {
    if (accountMode === 'existing') {
      bindingForm.setValue('accountId', selectedAccountId, { shouldValidate: true });
      setStep('model');
      return;
    }
    if (!newAccountProvider) {
      return;
    }
    setAccountBusy(true);
    setAccountError('');
    try {
      const localParams = localScopeQuery(selectedScope, scopeContext);
      const account = await adminApi.createAccount({
        ...(localParams.query ?? {}),
        providerCatalogId: newAccountProvider.id,
        label: newAccountLabel.trim(),
        apiKey: newAccountApiKey.trim() || undefined,
        baseUrl: canEditProviderBaseUrl(newAccountProvider) ? newAccountBaseUrl.trim() || undefined : undefined,
      });
      bindingForm.setValue('accountId', account.id, { shouldValidate: true });
      invalidateAll();
      setStep('model');
    } catch (err) {
      setAccountError(errorMessage(err, t('admin.aiPanel.messages.accountSaveFailed')));
    } finally {
      setAccountBusy(false);
    }
  };

  const submitBinding = bindingForm.handleSubmit(async (values) => {
    setBindingBusy(true);
    try {
      const resolved = resolveBindingForPurpose({
        purpose,
        selectedScope,
        bindingsForScope,
        instanceBindings,
        workspaceBindings,
      });
      const localScopeParams = compactScopeQuery(localScopeQuery(selectedScope, scopeContext).query);
      if (resolved.localBinding) {
        await adminApi.updateBinding(resolved.localBinding.id, {
          ...bindingParamsRequestBody(values),
          bindingState: 'active',
        });
      } else {
        await adminApi.createBinding({
          ...localScopeParams,
          scopeKind: selectedScope,
          bindingPurpose: purpose,
          ...bindingParamsRequestBody(values),
        });
      }
      toast.success(t('admin.aiPanel.messages.bindingSaved'));
      invalidateAll();
      onOpenChange(false);
    } catch (err) {
      toast.error(errorMessage(err, t('admin.aiPanel.messages.bindingSaveFailed')));
    } finally {
      setBindingBusy(false);
    }
  });

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <Sparkles className="h-4 w-4 text-primary" />
            {t('admin.aiWizard.title')}
          </DialogTitle>
          <DialogDescription>{t('admin.aiWizard.description')}</DialogDescription>
        </DialogHeader>

        <div className="space-y-5">
          <div className="grid gap-3 sm:grid-cols-2">
            <FormSelectField
              control={topForm.control}
              formState={topForm.formState}
              id="ai-wizard-purpose"
              label={t('admin.aiWizard.purposeLabel')}
              name="purpose"
              triggerClassName="h-9 text-sm"
            >
              {REQUIRED_RUNTIME_PURPOSE_ORDER.map((p) => (
                <SelectItem key={p} value={p}>
                  {purposeLabel(p, t)}
                </SelectItem>
              ))}
            </FormSelectField>
            <FormSelectField
              control={topForm.control}
              formState={topForm.formState}
              id="ai-wizard-scope"
              label={t('admin.aiWizard.scopeLabel')}
              name="scope"
              onValueChange={(value) => onScopeChange(value as AIScopeKind)}
              triggerClassName="h-9 text-sm"
            >
              {scopeOptions.map((option) => (
                <SelectItem key={option.kind} value={option.kind}>
                  {option.label}
                </SelectItem>
              ))}
            </FormSelectField>
          </div>

          <div className="flex items-center gap-2">
            <span className="section-label text-muted-foreground">
              {t('admin.aiWizard.stepNumber', { number: step === 'account' ? 1 : 2 })}
            </span>
            <span className="text-sm font-bold">
              {step === 'account' ? t('admin.aiWizard.steps.account.title') : t('admin.aiWizard.steps.model.title')}
            </span>
          </div>

          {step === 'account' ? (
            <div className="space-y-3">
              <div className="flex gap-2">
                <Button
                  type="button"
                  size="sm"
                  variant={accountMode === 'existing' ? 'default' : 'outline'}
                  onClick={() => setAccountMode('existing')}
                  disabled={activeAccounts.length === 0}
                >
                  {t('admin.aiWizard.useExistingAccount')}
                </Button>
                <Button
                  type="button"
                  size="sm"
                  variant={accountMode === 'new' ? 'default' : 'outline'}
                  onClick={() => setAccountMode('new')}
                >
                  <KeyRound className="mr-1.5 h-3.5 w-3.5" />
                  {t('admin.aiWizard.createNewAccount')}
                </Button>
              </div>

              {accountMode === 'existing' ? (
                <div className="space-y-2">
                  <Label htmlFor="ai-wizard-account">
                    {t('admin.aiPanel.fields.account')}
                  </Label>
                  <Select value={selectedAccountId} onValueChange={setSelectedAccountId}>
                    <SelectTrigger id="ai-wizard-account" className="h-9 text-sm">
                      <SelectValue placeholder={t('admin.aiPanel.placeholders.selectAccount')} />
                    </SelectTrigger>
                    <SelectContent>
                      {activeAccounts.map(entry => (
                        <SelectItem key={entry.id} value={entry.id}>{entry.label} · {entry.providerName}</SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                </div>
              ) : (
                <div className="space-y-3">
                  <div className="space-y-2">
                    <Label htmlFor="ai-wizard-new-account-provider">
                      {t('admin.provider')}
                    </Label>
                    <Select
                      value={newAccountProviderId}
                      onValueChange={value => {
                        const provider = providers.find(entry => entry.id === value) ?? null;
                        setNewAccountProviderId(value);
                        setNewAccountApiKey('');
                        setNewAccountBaseUrl(provider ? normalizeProviderBaseUrl(provider, provider.defaultBaseUrl) : '');
                      }}
                    >
                      <SelectTrigger id="ai-wizard-new-account-provider" className="h-9 text-sm">
                        <SelectValue placeholder={t('admin.aiPanel.placeholders.selectProvider')} />
                      </SelectTrigger>
                      <SelectContent>
                        {providers.map(entry => (
                          <SelectItem key={entry.id} value={entry.id}>{entry.displayName}</SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                  </div>
                  <div className="space-y-2">
                    <Label htmlFor="ai-wizard-new-account-label">
                      {t('admin.label')}
                    </Label>
                    <Input
                      id="ai-wizard-new-account-label"
                      value={newAccountLabel}
                      onChange={event => setNewAccountLabel(event.target.value)}
                      placeholder={t('admin.aiPanel.placeholders.accountLabel')}
                    />
                  </div>
                  {newAccountProvider && (
                    <ProviderCredentialFields
                      provider={newAccountProvider}
                      idPrefix="ai-wizard-new-account"
                      apiKey={newAccountApiKey}
                      baseUrl={newAccountBaseUrl}
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
                      }}
                      onApiKeyChange={setNewAccountApiKey}
                      onBaseUrlChange={setNewAccountBaseUrl}
                    />
                  )}
                </div>
              )}
              {accountError && (
                <p className="text-xs text-destructive">{accountError}</p>
              )}
            </div>
          ) : (
            <div className="space-y-3">
              <FormSelectField
                control={bindingForm.control}
                formState={bindingForm.formState}
                id="ai-wizard-model"
                label={t('admin.model')}
                name="modelCatalogId"
                placeholder={t('admin.aiPanel.placeholders.selectModel')}
                triggerClassName="h-9 text-sm"
              >
                {modelOptions.map(entry => {
                  const priceSuffix = formatModelPriceSuffix(resolveModelPriceSummary(entry.id, prices));
                  return (
                    <SelectItem key={entry.id} value={entry.id}>
                      {priceSuffix
                        ? t('admin.aiPanel.modelPricePerMillion', { model: entry.modelName, price: priceSuffix })
                        : entry.modelName}
                    </SelectItem>
                  );
                })}
              </FormSelectField>

              <details className="space-y-3">
                <summary className="cursor-pointer text-sm font-semibold">
                  {t('admin.aiPanel.fields.advancedSettings')}
                </summary>
                <div className="space-y-3 pt-3">
                  <FormTextareaField
                    formState={bindingForm.formState}
                    id="ai-wizard-system-prompt"
                    label={t('admin.systemPrompt')}
                    name="systemPrompt"
                    registration={bindingForm.register('systemPrompt')}
                    placeholder={t('admin.aiPanel.placeholders.systemPrompt')}
                    textareaClassName="min-h-[80px] text-sm"
                  />
                  <div className="grid gap-3 sm:grid-cols-3">
                    <FormInputField
                      formState={bindingForm.formState}
                      id="ai-wizard-temperature"
                      label={t('admin.temperature')}
                      name="temperature"
                      registration={bindingForm.register('temperature')}
                      placeholder={t('admin.aiPanel.placeholders.defaultValue')}
                    />
                    <FormInputField
                      formState={bindingForm.formState}
                      id="ai-wizard-top-p"
                      label={t('admin.topP')}
                      name="topP"
                      registration={bindingForm.register('topP')}
                      placeholder={t('admin.aiPanel.placeholders.defaultValue')}
                    />
                    <FormInputField
                      formState={bindingForm.formState}
                      id="ai-wizard-max-output-tokens"
                      label={t('admin.maxOutputTokens')}
                      name="maxOutputTokens"
                      registration={bindingForm.register('maxOutputTokens')}
                      placeholder={t('admin.aiPanel.placeholders.defaultValue')}
                    />
                  </div>
                  <FormTextareaField
                    formState={bindingForm.formState}
                    id="ai-wizard-extra-parameters"
                    label={t('admin.aiPanel.fields.parameters')}
                    name="extraParametersJson"
                    registration={bindingForm.register('extraParametersJson')}
                    placeholder={t('admin.aiPanel.placeholders.emptyJson')}
                    textareaClassName="min-h-[80px] font-mono text-xs"
                  />
                </div>
              </details>
            </div>
          )}
        </div>

        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>
            {t('admin.cancel')}
          </Button>
          {step === 'account' ? (
            <Button disabled={!canAdvanceFromAccount || accountBusy} onClick={() => void goToModelStep()}>
              {accountBusy ? <><Loader2 className="mr-1.5 h-3.5 w-3.5 animate-spin" /> {t('admin.saving')}</> : <>{t('admin.aiWizard.continue')}<ArrowRight className="ml-1.5 h-3.5 w-3.5" /></>}
            </Button>
          ) : (
            <>
              <Button variant="outline" onClick={() => setStep('account')}>
                <ArrowLeft className="mr-1.5 h-3.5 w-3.5" />
                {t('admin.aiWizard.back')}
              </Button>
              <Button
                disabled={!bindingForm.formState.isValid || bindingBusy}
                onClick={() => void submitBinding()}
              >
                {bindingBusy ? <><Loader2 className="mr-1.5 h-3.5 w-3.5 animate-spin" /> {t('admin.saving')}</> : t('admin.aiWizard.createBinding')}
              </Button>
            </>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
