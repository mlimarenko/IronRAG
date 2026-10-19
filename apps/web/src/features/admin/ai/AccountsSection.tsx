import { Plus } from 'lucide-react'
import {
  useEffect,
  useId,
  useState,
  type Dispatch,
  type FormEvent,
  type SetStateAction,
} from 'react'
import { useTranslation } from 'react-i18next'
import { toast } from 'sonner'

import { providerModeLabel } from '@/shared/lib/enumLabels'
import type { AIAccount, AIProvider, AIScopeKind } from '@/shared/types'
import { Badge, Button, Modal, Panel, SettingsList, SettingsListItem } from '@/shared/ui'
import type { AiScopeContext } from '../model/aiConfig'
import { accountStateLabel } from '../model/aiConfig'
import { screenPhase } from '@/shared/lib/screenPhase'
import { useAccountDetail, useAiAdminMutations } from './api'
import { errorText, scopeBody } from './utils'
import { aiScopeLabel } from './labels'

type AccountForm = {
  providerId: string
  label: string
  apiKey: string
  baseUrl: string
  credentialState: 'active' | 'invalid' | 'revoked'
  expectedUpdatedAt: string
}

const EMPTY_FORM: AccountForm = {
  providerId: '',
  label: '',
  apiKey: '',
  baseUrl: '',
  credentialState: 'active',
  expectedUpdatedAt: '',
}

function accountForm(account: AIAccount): AccountForm {
  return {
    providerId: account.providerId,
    label: account.label,
    apiKey: '',
    baseUrl: account.baseUrl ?? account.provider?.defaultBaseUrl ?? '',
    credentialState: account.state === 'unchecked' ? 'active' : account.state,
    expectedUpdatedAt: account.updatedAt,
  }
}

function providerBaseUrlIsEditable(provider: AIProvider | undefined) {
  return Boolean(
    provider &&
    provider.credentialPolicy.baseUrlMode !== 'fixed' &&
    provider.baseUrlPolicy.allowOverride,
  )
}

function isAccountLocalToScope(
  account: AIAccount,
  selectedScope: AIScopeKind,
  scopeContext: AiScopeContext,
) {
  if (account.scopeKind !== selectedScope) return false
  if (selectedScope === 'instance') return true
  if (selectedScope === 'workspace') {
    return Boolean(scopeContext.workspaceId) && account.workspaceId === scopeContext.workspaceId
  }
  return (
    Boolean(scopeContext.libraryId) &&
    account.libraryId === scopeContext.libraryId &&
    (!scopeContext.workspaceId || account.workspaceId === scopeContext.workspaceId)
  )
}

// An account is green only while it is active; `invalid` means the credential
// was rejected, and every other state (pending, disabled) is simply not in use.
function accountStateTone(state: AIAccount['state']): string {
  if (state === 'active') return 'ready'
  if (state === 'invalid') return 'failed'
  return 'neutral'
}

function CredentialFields({
  form,
  provider,
  editing,
  setForm,
}: Readonly<{
  form: AccountForm
  provider: AIProvider | undefined
  editing: boolean
  setForm: Dispatch<SetStateAction<Partial<AccountForm>>>
}>) {
  const { t } = useTranslation()
  if (!provider) return null
  const policy = provider.credentialPolicy
  const editableBaseUrl = providerBaseUrlIsEditable(provider)
  const baseUrlProtocol = provider.baseUrlPolicy.requireHttps ? 'HTTPS' : 'HTTP'
  const showBaseUrl = policy.baseUrlMode !== 'fixed' || Boolean(provider.defaultBaseUrl)
  return (
    <>
      <label className="field">
        <span>
          {policy.apiKeyRequired ? t('admin.apiKey') : t('admin.aiPanel.fields.tokenOptional')}
        </span>
        <input
          className="input"
          type="password"
          required={!editing && policy.apiKeyRequired}
          value={form.apiKey}
          placeholder={
            editing
              ? t('admin.aiPanel.placeholders.keepSecret')
              : t('admin.aiPanel.placeholders.providerToken')
          }
          onChange={(event) =>
            setForm((draft) => ({
              expectedUpdatedAt: draft.expectedUpdatedAt ?? form.expectedUpdatedAt,
              ...draft,
              apiKey: event.target.value,
            }))
          }
        />
        <small>{providerModeLabel(policy.validationMode, t)}</small>
      </label>
      {showBaseUrl && (
        <label className="field">
          <span>
            {policy.baseUrlRequired
              ? t('admin.aiPanel.fields.baseUrl')
              : t('admin.aiPanel.fields.baseUrlOptional')}
          </span>
          <input
            className="input"
            type="url"
            required={policy.baseUrlRequired && editableBaseUrl}
            disabled={!editableBaseUrl}
            value={editableBaseUrl ? form.baseUrl : (provider.defaultBaseUrl ?? '')}
            onChange={(event) =>
              setForm((draft) => ({
                expectedUpdatedAt: draft.expectedUpdatedAt ?? form.expectedUpdatedAt,
                ...draft,
                baseUrl: event.target.value,
              }))
            }
          />
          <small>
            {editableBaseUrl &&
              provider.baseUrlPolicy.allowPrivateNetwork &&
              t('admin.aiPanel.baseUrl.private', { protocol: baseUrlProtocol })}
            {editableBaseUrl &&
              !provider.baseUrlPolicy.allowPrivateNetwork &&
              t('admin.aiPanel.baseUrl.public', { protocol: baseUrlProtocol })}
            {!editableBaseUrl && t('admin.aiPanel.fields.fixedBaseUrlHint')}
          </small>
        </label>
      )}
      {Object.keys(provider.uiHints).length > 0 && (
        <details className="field field--wide aicfg-technical-details">
          <summary>{t('admin.aiPanel.technicalDetails')}</summary>
          <code>{JSON.stringify(provider.uiHints, null, 2)}</code>
        </details>
      )}
    </>
  )
}

export function AccountsSection({
  selectedScope,
  scopeContext,
  accounts,
  providers,
  requestCreate = 0,
  onCreateRequestConsumed,
}: Readonly<{
  selectedScope: AIScopeKind
  scopeContext: AiScopeContext
  accounts: AIAccount[]
  providers: AIProvider[]
  requestCreate?: number
  onCreateRequestConsumed?: () => void
}>) {
  const { t } = useTranslation()
  const mutations = useAiAdminMutations()
  const formId = useId()
  const [open, setOpen] = useState(requestCreate > 0)
  const [editingId, setEditingId] = useState<string | null>(null)
  const [formDraft, setFormDraft] = useState<Partial<AccountForm>>({})
  const [error, setError] = useState('')
  const detail = useAccountDetail(editingId, providers)
  const authoritativeAccount =
    editingId &&
    !detail.isPending &&
    !detail.isFetching &&
    !detail.isError &&
    detail.data?.id === editingId
      ? detail.data
      : undefined
  const form: AccountForm = {
    ...(authoritativeAccount ? accountForm(authoritativeAccount) : EMPTY_FORM),
    ...formDraft,
  }
  const selectedProvider = providers.find((provider) => provider.id === form.providerId)
  const editDetailReady = !editingId || Boolean(authoritativeAccount)
  // A failed detail request also leaves `editDetailReady` false, so the error
  // has to be subtracted from the loading condition — otherwise the form would
  // sit on a spinner that can never resolve.
  const editorPhase = screenPhase({
    loading: Boolean(editingId) && !editDetailReady && !detail.isError,
    failed: Boolean(editingId) && detail.isError,
  })

  useEffect(() => {
    if (requestCreate <= 0) return
    onCreateRequestConsumed?.()
  }, [onCreateRequestConsumed, requestCreate])

  const close = () => {
    setOpen(false)
    setEditingId(null)
    setFormDraft({})
    setError('')
  }
  const edit = (account: AIAccount) => {
    setEditingId(account.id)
    setFormDraft({})
    setOpen(true)
  }
  const selectProvider = (providerId: string) => {
    const provider = providers.find((entry) => entry.id === providerId)
    setFormDraft((draft) => ({
      expectedUpdatedAt: draft.expectedUpdatedAt ?? form.expectedUpdatedAt,
      ...draft,
      providerId,
      baseUrl: provider?.defaultBaseUrl ?? '',
    }))
  }
  const save = async (event: FormEvent) => {
    event.preventDefault()
    setError('')
    if (editingId && (!editDetailReady || !form.expectedUpdatedAt)) {
      setError(t('common.error'))
      return
    }
    if (
      editingId &&
      (!authoritativeAccount ||
        !isAccountLocalToScope(authoritativeAccount, selectedScope, scopeContext))
    ) {
      setError(t('admin.aiPanel.messages.accountMutationScopeChanged'))
      return
    }
    const provider = selectedProvider
    if (!provider) return
    const baseUrl = providerBaseUrlIsEditable(provider)
      ? form.baseUrl.trim() || undefined
      : undefined
    try {
      if (editingId) {
        const accountVersion = form.expectedUpdatedAt
        if (!accountVersion) {
          setError(t('common.error'))
          return
        }
        await mutations.accountUpdate.mutateAsync({
          id: editingId,
          body: {
            label: form.label.trim(),
            credentialState: form.credentialState,
            apiKey: form.apiKey.trim() || undefined,
            baseUrl,
            expectedUpdatedAt: accountVersion,
          },
        })
      } else {
        await mutations.accountCreate.mutateAsync({
          ...scopeBody(selectedScope, scopeContext),
          providerCatalogId: form.providerId,
          label: form.label.trim(),
          apiKey: form.apiKey.trim() || undefined,
          baseUrl,
        })
      }
      toast.success(t('admin.aiPanel.messages.accountSaved'))
      close()
    } catch (saveError) {
      setError(errorText(saveError, t('admin.aiPanel.messages.accountSaveFailed'), t))
    }
  }
  const remove = async (account: AIAccount) => {
    if (!isAccountLocalToScope(account, selectedScope, scopeContext)) {
      toast.error(t('admin.aiPanel.messages.accountMutationScopeChanged'))
      return
    }
    if (
      !window.confirm(
        t('admin.aiPanel.dialogs.deleteAccountDescription', { account: account.label }),
      )
    )
      return
    try {
      await mutations.accountDelete.mutateAsync(account.id)
    } catch (deleteError) {
      toast.error(errorText(deleteError, t('admin.aiPanel.messages.accountDeleteFailed'), t))
    }
  }

  return (
    <Panel className="settings-list-section">
      <div className="panel-heading">
        <div>
          <h2>{t('admin.aiPanel.accountsTitle')}</h2>
          <p>{t('admin.aiPanel.accountsDescription')}</p>
        </div>
        <Button variant="primary" onClick={() => setOpen(true)}>
          <Plus size={14} />
          {t('admin.addAccount')}
        </Button>
      </div>
      {accounts.length === 0 && (
        <div className="ai-empty">{t('admin.aiPanel.empty.noVisibleAccounts')}</div>
      )}
      <SettingsList label={t('admin.accounts')}>
        {accounts.map((account) => {
          const local = isAccountLocalToScope(account, selectedScope, scopeContext)
          return (
            <SettingsListItem
              key={account.id}
              title={account.label}
              description={account.providerName}
              summary={
                <>
                  <Badge tone={accountStateTone(account.state)}>
                    {accountStateLabel(account.state, t)}
                  </Badge>
                  <span>
                    {local
                      ? aiScopeLabel(account.scopeKind, t)
                      : t('admin.aiPanel.inheritedAccountReadOnly', {
                          scope: aiScopeLabel(account.scopeKind, t),
                        })}
                  </span>
                </>
              }
              actions={
                local && (
                  <Button
                    size="sm"
                    variant="ghost"
                    aria-label={`${t('admin.edit')}: ${account.label}`}
                    onClick={() => edit(account)}
                  >
                    {t('common.edit')}
                  </Button>
                )
              }
            >
              <dl className="ai-record-facts">
                <div>
                  <dt>{t('admin.aiPanel.fields.baseUrl')}</dt>
                  <dd>{account.baseUrl ?? account.provider?.defaultBaseUrl ?? '—'}</dd>
                </div>
                <div>
                  <dt>{t('admin.apiKey')}</dt>
                  <dd>{account.apiKeySummary || t('admin.aiPanel.tokenOptional')}</dd>
                </div>
              </dl>
              {local && (
                <Button
                  size="sm"
                  variant="danger"
                  aria-label={`${t('admin.delete')}: ${account.label}`}
                  onClick={() => void remove(account)}
                >
                  {t('admin.delete')}
                </Button>
              )}
            </SettingsListItem>
          )
        })}
      </SettingsList>
      <Modal
        open={open}
        onOpenChange={(next) => !next && close()}
        title={
          editingId
            ? t('admin.aiPanel.dialogs.editAccountTitle')
            : t('admin.aiPanel.dialogs.addAccountTitle')
        }
        description={t('admin.aiPanel.dialogs.accountDescription')}
        footer={
          <>
            <Button type="button" onClick={close}>
              {t('admin.cancel')}
            </Button>
            <Button
              variant="primary"
              type="submit"
              form={formId}
              disabled={
                !selectedProvider ||
                !editDetailReady ||
                mutations.accountCreate.isPending ||
                mutations.accountUpdate.isPending
              }
            >
              {t('admin.save')}
            </Button>
          </>
        }
      >
        <form id={formId} className="ai-form" onSubmit={(event) => void save(event)}>
          {editorPhase === 'error' && (
            <div className="ai-inline-error" role="alert">
              {t('common.error')}
            </div>
          )}
          {editorPhase === 'loading' && <div className="ai-empty">{t('admin.loading')}</div>}
          {editorPhase === 'ready' && (
            <div className="field-grid">
              <label className="field field--wide">
                <span>{t('admin.provider')}</span>
                <select
                  className="input"
                  required
                  disabled={Boolean(editingId)}
                  value={form.providerId}
                  onChange={(event) => selectProvider(event.target.value)}
                >
                  <option value="">{t('admin.selectProvider')}</option>
                  {providers
                    .filter(
                      (provider) =>
                        provider.lifecycleState === 'active' || provider.id === form.providerId,
                    )
                    .map((provider) => (
                      <option key={provider.id} value={provider.id}>
                        {provider.displayName}
                      </option>
                    ))}
                </select>
              </label>
              <label className="field field--wide">
                <span>{t('admin.label')}</span>
                <input
                  className="input"
                  required
                  value={form.label}
                  onChange={(event) =>
                    setFormDraft((draft) => ({
                      expectedUpdatedAt: draft.expectedUpdatedAt ?? form.expectedUpdatedAt,
                      ...draft,
                      label: event.target.value,
                    }))
                  }
                />
              </label>
              <CredentialFields
                form={form}
                provider={selectedProvider}
                editing={Boolean(editingId)}
                setForm={setFormDraft}
              />
              {editingId && (
                <label className="field field--wide">
                  <span>{t('admin.status')}</span>
                  <select
                    className="input"
                    value={form.credentialState}
                    onChange={(event) =>
                      setFormDraft((draft) => ({
                        expectedUpdatedAt: draft.expectedUpdatedAt ?? form.expectedUpdatedAt,
                        ...draft,
                        credentialState: event.target.value as AccountForm['credentialState'],
                      }))
                    }
                  >
                    {(['active', 'invalid', 'revoked'] as const).map((state) => (
                      <option key={state} value={state}>
                        {accountStateLabel(state, t)}
                      </option>
                    ))}
                  </select>
                </label>
              )}
            </div>
          )}
          {error && <p className="ai-inline-error">{error}</p>}
        </form>
      </Modal>
    </Panel>
  )
}
