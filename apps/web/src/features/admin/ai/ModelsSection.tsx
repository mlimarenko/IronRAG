import { Plus } from 'lucide-react'
import { useId, useState, type FormEvent } from 'react'
import { useTranslation } from 'react-i18next'
import { toast } from 'sonner'

import type { AiBindingPurpose } from '@/shared/api/generated'
import { useApp } from '@/shared/contexts/app-context'
import { lifecycleStateLabel, modelAvailabilityStateLabel } from '@/shared/lib/enumLabels'
import type { AIProvider } from '@/shared/types'
import { Badge, Button, Modal, Panel, SettingsList, SettingsListItem } from '@/shared/ui'
import type { AdminModelOption } from '../model/aiAdapter'
import { BINDING_PURPOSES, purposeLabel } from '../model/aiConfig'
import { useAiAdminMutations, useModelDetail } from './api'
import { PriceField } from './ModelFormSupport'
import { EMPTY_MODEL_FORM, formFromModel, pricingFromForm, type ModelForm } from './modelForm'
import { errorText } from './utils'
import { formatModelPricing } from './labels'

export function ModelsSection({
  models,
  providers,
}: Readonly<{
  models: AdminModelOption[]
  providers: AIProvider[]
}>) {
  const { t } = useTranslation()
  const { locale } = useApp()
  const mutations = useAiAdminMutations()
  const formId = useId()
  const [open, setOpen] = useState(false)
  const [editingId, setEditingId] = useState<string | null>(null)
  const [form, setForm] = useState<ModelForm>(EMPTY_MODEL_FORM)
  const [error, setError] = useState('')
  const detail = useModelDetail(editingId)

  const close = () => {
    setOpen(false)
    setEditingId(null)
    setForm(EMPTY_MODEL_FORM)
    setError('')
  }
  const edit = (model: AdminModelOption) => {
    setEditingId(model.id)
    setForm(formFromModel(model))
    setOpen(true)
  }
  const togglePurpose = (purpose: AiBindingPurpose) => {
    setForm((current) => ({
      ...current,
      purposes: current.purposes.includes(purpose)
        ? current.purposes.filter((entry) => entry !== purpose)
        : [...current.purposes, purpose],
    }))
  }
  const save = async (event: FormEvent) => {
    event.preventDefault()
    setError('')
    if (
      form.longEnabled &&
      [form.longThreshold, form.longInput, form.longCachedInput, form.longOutput].some(
        (value) => !value,
      )
    ) {
      setError(t('admin.aiPanel.messages.longContextRequired'))
      return
    }
    const common = {
      lifecycleState: form.lifecycleState,
      contextWindow: form.contextWindow ? Number(form.contextWindow) : null,
      maxOutputTokens: form.maxOutputTokens ? Number(form.maxOutputTokens) : null,
      allowedBindingPurposes: form.purposes,
      pricing: pricingFromForm(form),
    }
    try {
      if (editingId) await mutations.modelUpdate.mutateAsync({ id: editingId, body: common })
      else
        await mutations.modelCreate.mutateAsync({
          ...common,
          providerCatalogId: form.providerCatalogId,
          modelName: form.modelName.trim(),
        })
      toast.success(t('admin.aiPanel.messages.modelSaved'))
      close()
    } catch (saveError) {
      setError(errorText(saveError, t('admin.aiPanel.messages.modelSaveFailed'), t))
    }
  }
  const remove = async (model: AdminModelOption) => {
    if (
      !window.confirm(t('admin.aiPanel.dialogs.deleteModelDescription', { model: model.modelName }))
    )
      return
    try {
      await mutations.modelDelete.mutateAsync(model.id)
    } catch (deleteError) {
      toast.error(errorText(deleteError, t('admin.aiPanel.messages.modelDeleteFailed'), t))
    }
  }

  return (
    <Panel className="settings-list-section">
      <div className="panel-heading">
        <div>
          <h2>{t('admin.aiPanel.metrics.visibleModels')}</h2>
          <p>{t('admin.aiPanel.notices.seededPricesReset')}</p>
        </div>
        <Button variant="primary" onClick={() => setOpen(true)}>
          <Plus size={14} />
          {t('admin.aiPanel.actions.addModel')}
        </Button>
      </div>
      {models.length === 0 && (
        <div className="ai-empty">{t('admin.aiPanel.empty.noSelectableModels')}</div>
      )}
      <SettingsList label={t('admin.aiPanel.metrics.visibleModels')}>
        {models.map((model) => {
          const provider = providers.find((entry) => entry.id === model.providerCatalogId)
          return (
            <SettingsListItem
              key={model.id}
              title={model.modelName}
              description={provider?.displayName ?? model.providerCatalogId}
              summary={
                <>
                  <Badge tone={model.availabilityState === 'available' ? 'ready' : 'neutral'}>
                    {modelAvailabilityStateLabel(model.availabilityState, t)}
                  </Badge>
                  <span>
                    {t('admin.aiPanel.availableAccounts', {
                      accounts: model.availableAccountIds.length,
                    })}
                  </span>
                </>
              }
              actions={
                <Button
                  size="sm"
                  variant="ghost"
                  aria-label={`${t('admin.edit')}: ${model.modelName}`}
                  onClick={() => edit(model)}
                >
                  {t('common.edit')}
                </Button>
              }
            >
              <dl className="ai-record-facts">
                <div>
                  <dt>{t('admin.aiPanel.fields.purposes')}</dt>
                  <dd>
                    {model.allowedBindingPurposes
                      .map((purpose) => purposeLabel(purpose, t))
                      .join(' · ') || '—'}
                  </dd>
                </div>
                <div>
                  <dt>{t('admin.aiPanel.fields.pricing')}</dt>
                  <dd>{formatModelPricing(model.pricing, locale, t)}</dd>
                </div>
                <div>
                  <dt>{t('admin.aiPanel.fields.contextWindow')}</dt>
                  <dd>{model.contextWindow?.toLocaleString(locale) ?? '—'}</dd>
                </div>
              </dl>
              <Button
                size="sm"
                variant="danger"
                aria-label={`${t('admin.delete')}: ${model.modelName}`}
                onClick={() => void remove(model)}
              >
                {t('admin.delete')}
              </Button>
            </SettingsListItem>
          )
        })}
      </SettingsList>
      <Modal
        open={open}
        onOpenChange={(next) => !next && close()}
        title={
          editingId
            ? t('admin.aiPanel.dialogs.editModelTitle')
            : t('admin.aiPanel.dialogs.addModelTitle')
        }
        description={t('admin.aiPanel.dialogs.modelDescription')}
        wide
        footer={
          <>
            <Button type="button" onClick={close}>
              {t('admin.cancel')}
            </Button>
            <Button
              variant="primary"
              type="submit"
              form={formId}
              disabled={mutations.modelCreate.isPending || mutations.modelUpdate.isPending}
            >
              {t('admin.save')}
            </Button>
          </>
        }
      >
        <form id={formId} className="ai-form" onSubmit={(event) => void save(event)}>
          {editingId && detail.isPending ? (
            <div className="ai-empty">{t('admin.loading')}</div>
          ) : (
            <>
              <div className="field-grid">
                <label className="field">
                  <span>{t('admin.provider')}</span>
                  <select
                    className="input"
                    required
                    disabled={Boolean(editingId)}
                    value={form.providerCatalogId}
                    onChange={(event) =>
                      setForm({ ...form, providerCatalogId: event.target.value })
                    }
                  >
                    <option value="">{t('admin.selectProvider')}</option>
                    {providers.map((provider) => (
                      <option key={provider.id} value={provider.id}>
                        {provider.displayName}
                      </option>
                    ))}
                  </select>
                </label>
                <label className="field">
                  <span>{t('admin.model')}</span>
                  <input
                    className="input"
                    required
                    disabled={Boolean(editingId)}
                    value={form.modelName}
                    onChange={(event) => setForm({ ...form, modelName: event.target.value })}
                  />
                </label>
                <label className="field">
                  <span>{t('admin.aiPanel.fields.lifecycle')}</span>
                  <select
                    className="input"
                    value={form.lifecycleState}
                    onChange={(event) => setForm({ ...form, lifecycleState: event.target.value })}
                  >
                    {(['active', 'preview', 'deprecated', 'disabled'] as const).map((state) => (
                      <option key={state} value={state}>
                        {lifecycleStateLabel(state, t)}
                      </option>
                    ))}
                  </select>
                </label>
                <label className="field">
                  <span>{t('admin.aiPanel.fields.contextWindow')}</span>
                  <input
                    className="input"
                    type="number"
                    min="0"
                    value={form.contextWindow}
                    onChange={(event) => setForm({ ...form, contextWindow: event.target.value })}
                  />
                </label>
                <label className="field">
                  <span>{t('admin.maxOutputTokens')}</span>
                  <input
                    className="input"
                    type="number"
                    min="0"
                    value={form.maxOutputTokens}
                    onChange={(event) => setForm({ ...form, maxOutputTokens: event.target.value })}
                  />
                </label>
                <label className="field">
                  <span>{t('admin.aiPanel.fields.currency')}</span>
                  <input
                    className="input"
                    required
                    value={form.currency}
                    onChange={(event) => setForm({ ...form, currency: event.target.value })}
                  />
                </label>
                <PriceField
                  label={t('admin.aiPanel.fields.inputPricePer1m')}
                  value={form.input}
                  onChange={(input) => setForm({ ...form, input })}
                />
                <PriceField
                  label={t('admin.aiPanel.fields.cachedInputPricePer1m')}
                  value={form.cachedInput}
                  onChange={(cachedInput) => setForm({ ...form, cachedInput })}
                />
                <PriceField
                  label={t('admin.aiPanel.fields.cacheWriteInputPricePer1m')}
                  value={form.cacheWrite}
                  onChange={(cacheWrite) => setForm({ ...form, cacheWrite })}
                />
                <PriceField
                  label={t('admin.aiPanel.fields.outputPricePer1m')}
                  value={form.output}
                  onChange={(output) => setForm({ ...form, output })}
                />
              </div>
              <fieldset className="ai-check-grid">
                <legend>{t('admin.aiPanel.fields.purposes')}</legend>
                {BINDING_PURPOSES.map((purpose) => (
                  <label key={purpose}>
                    <input
                      type="checkbox"
                      checked={form.purposes.includes(purpose)}
                      onChange={() => togglePurpose(purpose)}
                    />
                    {purposeLabel(purpose, t)}
                  </label>
                ))}
              </fieldset>
              <label className="switch">
                <input
                  type="checkbox"
                  checked={form.longEnabled}
                  onChange={(event) => setForm({ ...form, longEnabled: event.target.checked })}
                />
                <span />
                {t('admin.aiPanel.fields.longContextPricing')}
              </label>
              {form.longEnabled && (
                <div className="field-grid ai-long-pricing">
                  <label className="field">
                    <span>{t('admin.aiPanel.fields.longContextThresholdTokens')}</span>
                    <input
                      className="input"
                      required
                      type="number"
                      min="0"
                      value={form.longThreshold}
                      onChange={(event) => setForm({ ...form, longThreshold: event.target.value })}
                    />
                  </label>
                  <PriceField
                    label={t('admin.aiPanel.fields.longContextInputPricePer1m')}
                    value={form.longInput}
                    onChange={(longInput) => setForm({ ...form, longInput })}
                  />
                  <PriceField
                    label={t('admin.aiPanel.fields.longContextCachedInputPricePer1m')}
                    value={form.longCachedInput}
                    onChange={(longCachedInput) => setForm({ ...form, longCachedInput })}
                  />
                  <PriceField
                    label={t('admin.aiPanel.fields.longContextOutputPricePer1m')}
                    value={form.longOutput}
                    onChange={(longOutput) => setForm({ ...form, longOutput })}
                  />
                </div>
              )}
              {error && <p className="ai-inline-error">{error}</p>}
            </>
          )}
        </form>
      </Modal>
    </Panel>
  )
}
