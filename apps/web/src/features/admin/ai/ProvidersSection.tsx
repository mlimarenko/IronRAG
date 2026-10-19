import { Plus } from 'lucide-react'
import { useId, useState, type FormEvent } from 'react'
import { useTranslation } from 'react-i18next'
import { toast } from 'sonner'

import {
  capabilityStateLabel,
  lifecycleStateLabel,
  providerCapabilityLabel,
  providerModeLabel,
} from '@/shared/lib/enumLabels'
import type { AiBindingPurpose, AIAccount, AIModelOption, AIProvider } from '@/shared/types'
import { purposeLabel } from '../model/aiConfig'
import { Badge, Button, Modal, Panel, SettingsList, SettingsListItem } from '@/shared/ui'
import { useAiAdminMutations, useProviderDetail } from './api'
import { errorText, parseJsonObject } from './utils'

type ProviderForm = {
  displayName: string
  providerKind: string
  apiStyle: string
  lifecycleState: AIProvider['lifecycleState']
  defaultBaseUrl: string
  profileJson: string
}

const EMPTY_FORM: ProviderForm = {
  displayName: '',
  providerKind: '',
  apiStyle: '',
  lifecycleState: 'active',
  defaultBaseUrl: '',
  profileJson: '',
}

function providerProfile(provider: AIProvider) {
  return {
    credentials: provider.credentialPolicy,
    baseUrl: provider.baseUrlPolicy,
    modelDiscovery: provider.modelDiscovery,
    capabilities: provider.capabilities,
    runtime: provider.runtime,
    uiHints: provider.uiHints,
  }
}

function ProviderMetadata({ provider }: Readonly<{ provider: AIProvider }>) {
  const { t } = useTranslation()
  return (
    <div className="ai-provider-profile">
      <div>
        <span>{t('admin.aiPanel.fields.providerKind')}</span>
        <strong>{provider.kind}</strong>
      </div>
      <div>
        <span>{t('admin.aiPanel.fields.apiStyle')}</span>
        <strong>{provider.apiStyle}</strong>
      </div>
      <div>
        <span>{t('admin.aiPanel.fields.requirements')}</span>
        <strong>
          {provider.credentialPolicy.apiKeyRequired
            ? t('admin.apiKey')
            : t('admin.aiPanel.tokenOptional')}
          {' · '}
          {providerModeLabel(provider.credentialPolicy.baseUrlMode, t)}
        </strong>
      </div>
      <div>
        <span>{t('admin.aiPanel.fields.modelDiscovery')}</span>
        <strong>{providerModeLabel(provider.modelDiscovery.mode, t)}</strong>
      </div>
      <div className="ai-capabilities">
        {Object.entries(provider.capabilities).map(([name, state]) => (
          <span key={name} title={capabilityStateLabel(state ?? 'unknown', t)}>
            <Badge tone={state === 'supported' ? 'ready' : 'neutral'}>
              {providerCapabilityLabel(name, t)}
            </Badge>
          </span>
        ))}
      </div>
      {/* Split by kind, because the two read as opposites. "Cannot serve" is a
          dead end the operator has to route around; "you choose the model" is
          an invitation, and labelling it as unsupported is how a router came to
          look broken for chat it serves perfectly well. */}
      {(['provider_cannot', 'operator_supplies'] as const).map((kind) => {
        const gaps = provider.purposeGaps.filter((gap) => gap.kind === kind)
        if (gaps.length === 0) return null
        return (
          <div className="ai-provider-gaps" key={kind}>
            <span>{t(`admin.aiPanel.fields.purposeGaps.${kind}`)}</span>
            <ul>
              {gaps.map((gap) => (
                <li key={gap.purpose}>
                  <strong>{purposeLabel(gap.purpose as AiBindingPurpose, t)}</strong>
                  {': '}
                  {t(`admin.aiPanel.unsupportedReasons.${gap.code}`, {
                    defaultValue: gap.reason,
                  })}
                </li>
              ))}
            </ul>
          </div>
        )
      })}
      {Object.keys(provider.uiHints).length > 0 && (
        <details className="aicfg-technical-details">
          <summary>{t('admin.aiPanel.technicalDetails')}</summary>
          <pre>{JSON.stringify(providerProfile(provider), null, 2)}</pre>
        </details>
      )}
    </div>
  )
}

export function ProvidersSection({
  providers,
  models,
  accounts,
}: Readonly<{
  providers: AIProvider[]
  models: AIModelOption[]
  accounts: AIAccount[]
}>) {
  const { t } = useTranslation()
  const mutations = useAiAdminMutations()
  const formId = useId()
  const [open, setOpen] = useState(false)
  const [editingId, setEditingId] = useState<string | null>(null)
  const [form, setForm] = useState<ProviderForm>(EMPTY_FORM)
  const [error, setError] = useState('')
  const detail = useProviderDetail(editingId)

  const close = () => {
    setOpen(false)
    setEditingId(null)
    setForm(EMPTY_FORM)
    setError('')
  }
  const edit = (provider: AIProvider) => {
    setEditingId(provider.id)
    setForm({
      displayName: provider.displayName,
      providerKind: provider.kind,
      apiStyle: provider.apiStyle,
      lifecycleState: provider.lifecycleState,
      defaultBaseUrl: provider.defaultBaseUrl ?? '',
      profileJson: JSON.stringify(providerProfile(provider), null, 2),
    })
    setOpen(true)
  }
  const save = async (event: FormEvent) => {
    event.preventDefault()
    setError('')
    try {
      const profile = parseJsonObject(form.profileJson)
      if (!profile) throw new Error(t('admin.aiPanel.messages.invalidJson'))
      const body = {
        displayName: form.displayName.trim(),
        providerKind: form.providerKind.trim(),
        apiStyle: form.apiStyle.trim(),
        lifecycleState: form.lifecycleState,
        defaultBaseUrl: form.defaultBaseUrl.trim() || null,
        capabilityFlagsJson: profile,
      }
      if (editingId) await mutations.providerUpdate.mutateAsync({ id: editingId, body })
      else await mutations.providerCreate.mutateAsync(body)
      toast.success(t('admin.aiPanel.messages.providerSaved'))
      close()
    } catch (saveError) {
      setError(errorText(saveError, t('admin.aiPanel.messages.providerSaveFailed'), t))
    }
  }
  const remove = async (provider: AIProvider) => {
    if (
      !window.confirm(
        t('admin.aiPanel.dialogs.deleteProviderDescription', { provider: provider.displayName }),
      )
    )
      return
    try {
      await mutations.providerDelete.mutateAsync(provider.id)
    } catch (deleteError) {
      toast.error(errorText(deleteError, t('admin.aiPanel.messages.providerDeleteFailed'), t))
    }
  }

  return (
    <Panel className="settings-list-section">
      <div className="panel-heading">
        <div>
          <h2>{t('admin.providers')}</h2>
          <p>{t('admin.aiPanel.dialogs.providerDescription')}</p>
        </div>
        <Button variant="primary" onClick={() => setOpen(true)}>
          <Plus size={14} />
          {t('admin.aiPanel.actions.addProvider')}
        </Button>
      </div>
      {providers.length === 0 && <div className="ai-empty">{t('admin.noProviders')}</div>}
      <SettingsList label={t('admin.providers')}>
        {providers.map((provider) => (
          <SettingsListItem
            key={provider.id}
            title={provider.displayName}
            description={
              <span>
                {t('admin.aiPanel.providerUsage', {
                  models: t('admin.aiPanel.providerUsageModels', {
                    count: models.filter((model) => model.providerCatalogId === provider.id).length,
                  }),
                  accounts: t('admin.aiPanel.providerUsageAccounts', {
                    count: accounts.filter((account) => account.providerId === provider.id).length,
                  }),
                })}
              </span>
            }
            summary={
              <Badge tone={provider.lifecycleState === 'active' ? 'ready' : 'neutral'}>
                {lifecycleStateLabel(provider.lifecycleState, t)}
              </Badge>
            }
            actions={
              <Button
                size="sm"
                variant="ghost"
                aria-label={`${t('admin.edit')}: ${provider.displayName}`}
                onClick={() => edit(provider)}
              >
                {t('common.edit')}
              </Button>
            }
          >
            <ProviderMetadata provider={provider} />
            <Button
              size="sm"
              variant="danger"
              aria-label={`${t('admin.delete')}: ${provider.displayName}`}
              onClick={() => void remove(provider)}
            >
              {t('admin.delete')}
            </Button>
          </SettingsListItem>
        ))}
      </SettingsList>
      <Modal
        open={open}
        onOpenChange={(next) => !next && close()}
        title={
          editingId
            ? t('admin.aiPanel.dialogs.editProviderTitle')
            : t('admin.aiPanel.dialogs.addProviderTitle')
        }
        description={t('admin.aiPanel.dialogs.providerDescription')}
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
              disabled={mutations.providerCreate.isPending || mutations.providerUpdate.isPending}
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
                  <span>{t('admin.aiPanel.fields.displayName')}</span>
                  <input
                    className="input"
                    required
                    value={form.displayName}
                    onChange={(event) => setForm({ ...form, displayName: event.target.value })}
                  />
                </label>
                <label className="field">
                  <span>{t('admin.aiPanel.fields.providerKind')}</span>
                  <input
                    className="input"
                    required
                    value={form.providerKind}
                    onChange={(event) => setForm({ ...form, providerKind: event.target.value })}
                  />
                </label>
                <label className="field">
                  <span>{t('admin.aiPanel.fields.apiStyle')}</span>
                  <input
                    className="input"
                    required
                    value={form.apiStyle}
                    onChange={(event) => setForm({ ...form, apiStyle: event.target.value })}
                  />
                </label>
                <label className="field">
                  <span>{t('admin.aiPanel.fields.lifecycle')}</span>
                  <select
                    className="input"
                    value={form.lifecycleState}
                    onChange={(event) =>
                      setForm({
                        ...form,
                        lifecycleState: event.target.value as ProviderForm['lifecycleState'],
                      })
                    }
                  >
                    {(['active', 'preview', 'deprecated', 'disabled'] as const).map((state) => (
                      <option key={state} value={state}>
                        {lifecycleStateLabel(state, t)}
                      </option>
                    ))}
                  </select>
                </label>
                <label className="field field--wide">
                  <span>{t('admin.aiPanel.fields.baseUrlOptional')}</span>
                  <input
                    className="input"
                    value={form.defaultBaseUrl}
                    onChange={(event) => setForm({ ...form, defaultBaseUrl: event.target.value })}
                  />
                </label>
                <label className="field field--wide">
                  <span>{t('admin.aiPanel.fields.providerProfileJson')}</span>
                  <textarea
                    className="input code-input"
                    required
                    rows={14}
                    value={form.profileJson}
                    onChange={(event) => setForm({ ...form, profileJson: event.target.value })}
                  />
                </label>
              </div>
              {detail.data && <ProviderMetadata provider={detail.data} />}
              {error && <p className="ai-inline-error">{error}</p>}
            </>
          )}
        </form>
      </Modal>
    </Panel>
  )
}
