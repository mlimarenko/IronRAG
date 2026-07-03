import { useTranslation } from 'react-i18next';
import type { TFunction } from 'i18next';
import { AlertTriangle, Brain, KeyRound, Loader2, Settings2 } from 'lucide-react';

import { Button } from '@/shared/components/ui/button';
import { SelectItem } from '@/shared/components/ui/select';
import { StatusBadge } from '@/shared/components/StatusBadge';
import { FormInputField, FormSelectField, FormTextareaField, type TypedFormReturn } from '@/shared/forms';
import type { AIAccount, AIModelOption, AIPurpose, AIScopeKind, PricingRule } from '@/shared/types';
import {
  formatModelPriceSuffix,
  isModelAvailableForAccount,
  purposeLabel as translatedPurposeLabel,
  resolveModelPriceSummary,
  scopeLabel as translatedScopeLabel,
  type AccountModelLoadState,
  type BindingResolution,
  type bindingParamsSchema,
} from '@/features/admin/model/aiConfig';
import { shouldRefreshCredentialModels } from '@/shared/lib/ai-provider';

type BindingParamsForm = TypedFormReturn<ReturnType<typeof bindingParamsSchema>>;

type BindingPurposeCardProps = {
  purpose: AIPurpose;
  selectedScope: AIScopeKind;
  resolved: BindingResolution;
  availableAccounts: AIAccount[];
  models: AIModelOption[];
  prices: PricingRule[];
  modelById: Map<string, AIModelOption>;
  modelsByAccountId: Record<string, AIModelOption[]>;
  selectedAccount: AIAccount | null;
  selectedAccountLoadState: AccountModelLoadState | undefined;
  editing: boolean;
  form: BindingParamsForm;
  bindingSaving: boolean;
  onAccountChange: (value: string) => void;
  onOpen: () => void;
  onCancel: () => void;
  onSave: () => void;
  onReset: () => void;
};

function modelOptionLabel(model: AIModelOption, prices: PricingRule[], t: TFunction) {
  const priceSuffix = formatModelPriceSuffix(resolveModelPriceSummary(model.id, prices));
  return priceSuffix
    ? t('admin.aiPanel.modelPricePerMillion', { model: model.modelName, price: priceSuffix })
    : model.modelName;
}

export function BindingPurposeCard({
  purpose,
  selectedScope,
  resolved,
  availableAccounts,
  models,
  prices,
  modelById,
  modelsByAccountId,
  selectedAccount,
  selectedAccountLoadState,
  editing,
  form,
  bindingSaving,
  onAccountChange,
  onOpen,
  onCancel,
  onSave,
  onReset,
}: BindingPurposeCardProps) {
  const { t } = useTranslation();
  const purposeLabel = (value: AIPurpose) => translatedPurposeLabel(value, t);
  const scopeLabel = (value: AIScopeKind) => translatedScopeLabel(value, t);
  const account = availableAccounts.find(entry => entry.id === resolved.effectiveBinding?.accountId);
  const model = resolved.effectiveBinding ? modelById.get(resolved.effectiveBinding.modelCatalogId) : undefined;
  const bindingModelUnavailable =
    account && model
      ? !isModelAvailableForAccount(model, account, modelsByAccountId)
      : false;
  const selectedAccountRequiresModelDiscovery =
    shouldRefreshCredentialModels(selectedAccount?.provider);
  const selectedAccountModelDiscoveryPending =
    selectedAccountRequiresModelDiscovery && selectedAccountLoadState !== 'ready';
  const modelOptions = models
    .filter(entry => entry.allowedBindingPurposes.includes(purpose))
    .filter(entry => !selectedAccount || entry.providerCatalogId === selectedAccount.providerId);
  const selectedModelId = form.watch('modelCatalogId');
  const selectedModel = models.find(entry => entry.id === selectedModelId);
  const selectedModelUnavailable =
    selectedAccount !== null
    && selectedModelId !== ''
    && (
      selectedAccountModelDiscoveryPending
      || !isModelAvailableForAccount(selectedModel, selectedAccount, modelsByAccountId)
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
            <StatusBadge tone={resolved.sourceKind === 'instance' ? 'ready' : 'warning'} className="mt-1">
              {t('admin.aiPanel.labels.inheritedFrom', { scope: scopeLabel(resolved.sourceKind) })}
            </StatusBadge>
          )}
        </div>

        {resolved.effectiveBinding && account && model ? (
          <div className="col-span-2 min-w-0 space-y-0.5 text-sm lg:col-span-1">
            <div className="flex items-center gap-2">
              <KeyRound className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
              <span className="min-w-0 font-semibold [overflow-wrap:anywhere]">{account.label}</span>
              <span className="min-w-0 text-xs text-muted-foreground [overflow-wrap:anywhere]">· {account.providerName}</span>
            </div>
            <div className="flex items-center gap-2">
              <Brain className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
              <span className="min-w-0 font-semibold [overflow-wrap:anywhere]">{model.modelName}</span>
              {formatModelPriceSuffix(resolveModelPriceSummary(model.id, prices)) && (
                <span className="min-w-0 text-xs text-muted-foreground [overflow-wrap:anywhere]">
                  · {formatModelPriceSuffix(resolveModelPriceSummary(model.id, prices))}
                </span>
              )}
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
          {t('admin.aiPanel.messages.bindingModelUnavailable', { model: model?.modelName ?? '' })}
        </div>
      )}

      {editing && (
        <div className="mt-3 rounded-md bg-surface-sunken p-3 space-y-3">
          <div className="grid gap-3 xl:grid-cols-2">
            <FormSelectField
              control={form.control}
              formState={form.formState}
              id={`binding-${purpose}-account`}
              label={t('admin.aiPanel.fields.account')}
              name="accountId"
              onValueChange={onAccountChange}
              placeholder={t('admin.aiPanel.placeholders.selectAccount')}
              triggerClassName="h-9 text-sm"
            >
              {availableAccounts.map(entry => (
                <SelectItem key={entry.id} value={entry.id}>
                  {entry.label} · {scopeLabel(entry.scopeKind)}
                </SelectItem>
              ))}
            </FormSelectField>
            <FormSelectField
              control={form.control}
              formState={form.formState}
              id={`binding-${purpose}-model`}
              label={t('admin.model')}
              name="modelCatalogId"
              placeholder={t('admin.aiPanel.placeholders.selectModel')}
              triggerClassName="h-9 text-sm"
            >
              {modelOptions.map(entry => (
                <SelectItem key={entry.id} value={entry.id}>
                  {modelOptionLabel(entry, prices, t)}
                </SelectItem>
              ))}
            </FormSelectField>
          </div>

          {selectedAccountLoadState === 'loading' && (
            <div className="flex items-center gap-2 text-sm text-muted-foreground">
              <Loader2 className="h-4 w-4 animate-spin" />
              {t('admin.aiPanel.messages.checkingAccountModels')}
            </div>
          )}
          {selectedModelUnavailable && (
            <div className="flex items-center gap-2 text-sm text-status-warning">
              <AlertTriangle className="h-4 w-4" />
              {t('admin.aiPanel.messages.selectedModelUnavailable')}
            </div>
          )}

          <details className="space-y-3">
            <summary className="cursor-pointer text-sm font-semibold">
              {t('admin.aiPanel.fields.advancedSettings')}
            </summary>
            <div className="space-y-3 pt-3">
              <FormTextareaField
                formState={form.formState}
                id={`binding-${purpose}-system-prompt`}
                label={t('admin.systemPrompt')}
                name="systemPrompt"
                registration={form.register('systemPrompt')}
                placeholder={t('admin.aiPanel.placeholders.systemPrompt')}
                textareaClassName="min-h-[80px] text-sm"
              />
              <div className="grid gap-3 sm:grid-cols-3">
                <FormInputField
                  formState={form.formState}
                  id={`binding-${purpose}-temperature`}
                  label={t('admin.temperature')}
                  name="temperature"
                  registration={form.register('temperature')}
                  placeholder={t('admin.aiPanel.placeholders.defaultValue')}
                />
                <FormInputField
                  formState={form.formState}
                  id={`binding-${purpose}-top-p`}
                  label={t('admin.topP')}
                  name="topP"
                  registration={form.register('topP')}
                  placeholder={t('admin.aiPanel.placeholders.defaultValue')}
                />
                <FormInputField
                  formState={form.formState}
                  id={`binding-${purpose}-max-output-tokens`}
                  label={t('admin.maxOutputTokens')}
                  name="maxOutputTokens"
                  registration={form.register('maxOutputTokens')}
                  placeholder={t('admin.aiPanel.placeholders.defaultValue')}
                />
              </div>
              <FormTextareaField
                formState={form.formState}
                id={`binding-${purpose}-extra-parameters`}
                label={t('admin.aiPanel.fields.parameters')}
                name="extraParametersJson"
                registration={form.register('extraParametersJson')}
                placeholder={t('admin.aiPanel.placeholders.emptyJson')}
                textareaClassName="min-h-[80px] font-mono text-xs"
              />
            </div>
          </details>

          <div className="flex flex-wrap gap-2">
            <Button size="sm" disabled={!form.formState.isValid || bindingSaving || Boolean(selectedModelUnavailable)} onClick={onSave}>
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
