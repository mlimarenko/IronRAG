import { ArrowLeft, ArrowRight } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { toast } from 'sonner'

import type { AiBindingPurpose } from '@/shared/api/generated'
import { formatCost } from '@/shared/format'
import type { AIAccount, AIProvider, AIScopeKind } from '@/shared/types'
import { Button, Modal, Segmented } from '@/shared/ui'
import type { AdminModelOption } from '../model/aiAdapter'
import {
  BINDING_PURPOSES,
  purposeDescription,
  purposeLabel,
  type AiScopeContext,
} from '../model/aiConfig'
import { aiScopeLabel } from './labels'
import { useAiAdminMutations } from './api'
import { errorText, scopeBody } from './utils'

type AccountMode = 'existing' | 'new'

function formatPrice(amount: string, currencyCode: string, locale: string) {
  return formatCost(
    {
      costRollupFreshness: 'current',
      totalCost: amount,
      currencyCode,
      providerCallCount: 0,
      unpricedProviderCallCount: 0,
      costCoverage: 'complete',
    },
    '',
    '',
    locale,
    6,
  )
}

// The step list marks exactly one step active; everything before it is done
// and everything after it carries no state class at all.
function wizardStepClass(index: number, step: number): string {
  if (index === step) return 'is-active'
  return index < step ? 'is-done' : ''
}

export function AiWizard({
  open,
  onOpenChange,
  selectedScope,
  onScopeChange,
  scopeContext,
  accounts,
  providers,
  models,
}: Readonly<{
  open: boolean
  onOpenChange: (open: boolean) => void
  selectedScope: AIScopeKind
  onScopeChange: (scope: AIScopeKind) => void
  scopeContext: AiScopeContext
  accounts: AIAccount[]
  providers: AIProvider[]
  models: AdminModelOption[]
}>) {
  const { t, i18n } = useTranslation()
  const mutations = useAiAdminMutations()
  const [step, setStep] = useState(0)
  const [purpose, setPurpose] = useState<AiBindingPurpose>('query_answer')
  const [mode, setMode] = useState<AccountMode>('existing')
  const [accountId, setAccountId] = useState('')
  const [providerId, setProviderId] = useState('')
  const [label, setLabel] = useState('')
  const [apiKey, setApiKey] = useState('')
  const [baseUrl, setBaseUrl] = useState('')
  const [modelId, setModelId] = useState('')
  const [error, setError] = useState('')
  const provider = providers.find((entry) => entry.id === providerId)
  const account = accounts.find((entry) => entry.id === accountId)
  const targetProviderId = mode === 'existing' ? account?.providerId : providerId
  const modelOptions = models.filter(
    (model) =>
      model.allowedBindingPurposes.includes(purpose) &&
      model.providerCatalogId === targetProviderId &&
      model.availabilityState !== 'unavailable' &&
      (mode === 'new' || model.availableAccountIds.includes(accountId)),
  )
  const canContinue = [
    Boolean(purpose),
    selectedScope === 'instance' ||
      (Boolean(scopeContext.workspaceId) &&
        (selectedScope !== 'library' || Boolean(scopeContext.libraryId))),
    mode === 'existing'
      ? Boolean(accountId)
      : Boolean(
          provider && label.trim() && (!provider.credentialPolicy.apiKeyRequired || apiKey.trim()),
        ),
    Boolean(modelId),
  ][step]

  const close = () => {
    onOpenChange(false)
    setStep(0)
    setError('')
  }
  const create = async () => {
    setError('')
    try {
      let selectedAccountId = accountId
      if (mode === 'new' && provider) {
        const editableBaseUrl =
          provider.credentialPolicy.baseUrlMode !== 'fixed' && provider.baseUrlPolicy.allowOverride
        const created = await mutations.accountCreate.mutateAsync({
          ...scopeBody(selectedScope, scopeContext),
          providerCatalogId: provider.id,
          label: label.trim(),
          apiKey: apiKey.trim() || undefined,
          baseUrl: editableBaseUrl ? baseUrl.trim() || undefined : undefined,
        })
        selectedAccountId = created.id
      }
      await mutations.bindingCreate.mutateAsync({
        ...scopeBody(selectedScope, scopeContext),
        bindingPurpose: purpose,
        accountId: selectedAccountId,
        modelCatalogId: modelId,
      })
      toast.success(t('admin.aiPanel.messages.bindingSaved'))
      close()
    } catch (createError) {
      setError(errorText(createError, t('admin.aiPanel.messages.bindingSaveFailed'), t))
    }
  }

  const content = [
    <div className="ai-wizard-options" key="purpose">
      {BINDING_PURPOSES.map((entry) => (
        <button
          type="button"
          className={purpose === entry ? 'is-active' : ''}
          key={entry}
          onClick={() => setPurpose(entry)}
        >
          <strong>{purposeLabel(entry, t)}</strong>
          <small>{purposeDescription(entry, t)}</small>
        </button>
      ))}
    </div>,
    <div className="ai-wizard-scope" key="scope">
      <Segmented
        value={selectedScope}
        onChange={(value) => onScopeChange(value as AIScopeKind)}
        label={t('admin.scope')}
        items={(['instance', 'workspace', 'library'] as const).map((scope) => ({
          value: scope,
          label: aiScopeLabel(scope, t),
        }))}
      />
      <p>{t('admin.aiPanel.inheritanceTooltip')}</p>
    </div>,
    <div key="account">
      <Segmented
        value={mode}
        onChange={(value) => {
          setMode(value as AccountMode)
          setModelId('')
        }}
        label={t('admin.accounts')}
        items={[
          { value: 'existing', label: t('admin.aiWizard.useExistingAccount') },
          { value: 'new', label: t('admin.aiWizard.createNewAccount') },
        ]}
      />
      {mode === 'existing' ? (
        <label className="field">
          <span>{t('admin.accounts')}</span>
          <select
            className="input"
            value={accountId}
            onChange={(event) => {
              setAccountId(event.target.value)
              setModelId('')
            }}
          >
            <option value="">{t('admin.aiPanel.placeholders.selectAccount')}</option>
            {accounts
              .filter((entry) => entry.state === 'active')
              .map((entry) => (
                <option key={entry.id} value={entry.id}>
                  {entry.label}
                </option>
              ))}
          </select>
        </label>
      ) : (
        <div className="field-grid ai-wizard-new-account">
          <label className="field">
            <span>{t('admin.provider')}</span>
            <select
              className="input"
              value={providerId}
              onChange={(event) => {
                const next = providers.find((entry) => entry.id === event.target.value)
                setProviderId(event.target.value)
                setBaseUrl(next?.defaultBaseUrl ?? '')
                setModelId('')
              }}
            >
              <option value="">{t('admin.selectProvider')}</option>
              {providers
                .filter((entry) => entry.lifecycleState === 'active')
                .map((entry) => (
                  <option key={entry.id} value={entry.id}>
                    {entry.displayName}
                  </option>
                ))}
            </select>
          </label>
          <label className="field">
            <span>{t('admin.label')}</span>
            <input
              className="input"
              value={label}
              onChange={(event) => setLabel(event.target.value)}
            />
          </label>
          {provider && (
            <>
              <label className="field">
                <span>
                  {provider.credentialPolicy.apiKeyRequired
                    ? t('admin.apiKey')
                    : t('admin.aiPanel.fields.tokenOptional')}
                </span>
                <input
                  className="input"
                  type="password"
                  value={apiKey}
                  onChange={(event) => setApiKey(event.target.value)}
                />
              </label>
              {provider.credentialPolicy.baseUrlMode !== 'fixed' && (
                <label className="field">
                  <span>{t('admin.aiPanel.fields.baseUrl')}</span>
                  <input
                    className="input"
                    disabled={!provider.baseUrlPolicy.allowOverride}
                    required={provider.credentialPolicy.baseUrlRequired}
                    value={baseUrl}
                    onChange={(event) => setBaseUrl(event.target.value)}
                  />
                  {!provider.baseUrlPolicy.allowOverride && (
                    <small>{t('admin.aiPanel.fields.fixedBaseUrlHint')}</small>
                  )}
                </label>
              )}
            </>
          )}
        </div>
      )}
    </div>,
    <div key="model" className="ai-wizard-options">
      {modelOptions.map((model) => (
        <button
          type="button"
          className={modelId === model.id ? 'is-active' : ''}
          key={model.id}
          onClick={() => setModelId(model.id)}
        >
          <strong>{model.modelName}</strong>
          <small>
            {t('admin.aiPanel.pricing.summary', {
              input: formatPrice(
                model.pricing.inputPricePer1m,
                model.pricing.currencyCode,
                i18n.language,
              ),
              output: formatPrice(
                model.pricing.outputPricePer1m,
                model.pricing.currencyCode,
                i18n.language,
              ),
            })}
          </small>
        </button>
      ))}
      {modelOptions.length === 0 && <p>{t('admin.aiPanel.empty.noSelectableModels')}</p>}
    </div>,
  ][step]

  return (
    <Modal
      open={open}
      onOpenChange={(next) => (next ? onOpenChange(true) : close())}
      title={t('admin.aiWizard.title')}
      description={t('admin.aiWizard.description')}
      wide
      footer={
        <>
          <Button
            type="button"
            disabled={step === 0}
            onClick={() => setStep((current) => current - 1)}
          >
            <ArrowLeft size={14} />
            {t('admin.aiWizard.back')}
          </Button>
          {step < 3 ? (
            <Button
              variant="primary"
              type="button"
              disabled={!canContinue}
              onClick={() => setStep((current) => current + 1)}
            >
              {t('admin.aiWizard.continue')}
              <ArrowRight size={14} />
            </Button>
          ) : (
            <Button
              variant="primary"
              type="button"
              disabled={
                !canContinue ||
                mutations.bindingCreate.isPending ||
                mutations.accountCreate.isPending
              }
              onClick={() => void create()}
            >
              {t('admin.aiWizard.createBinding')}
              <ArrowRight size={14} />
            </Button>
          )}
        </>
      }
    >
      <ol className="wizard-steps">
        {['purpose', 'scope', 'account', 'model'].map((name, index) => (
          <li key={name} className={wizardStepClass(index, step)}>
            <span>{index + 1}</span>
            <div>
              <strong>{t(`admin.aiWizard.steps.${name}.title`)}</strong>
            </div>
          </li>
        ))}
      </ol>
      <div className="ai-wizard-body">{content}</div>
      {error && <p className="ai-inline-error">{error}</p>}
    </Modal>
  )
}
