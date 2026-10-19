import { useId, useMemo, useState, type FormEvent } from 'react'
import { useTranslation } from 'react-i18next'
import { toast } from 'sonner'

import type { AiBindingPurpose } from '@/shared/api/generated'
import { getApiProblem } from '@/shared/api'
import { bindingStateLabel } from '@/shared/lib/enumLabels'
import type { AIAccount, AIBinding, AIBindingState, AIScopeKind } from '@/shared/types'
import { Badge, Button, Modal, Panel, SettingsList, SettingsListItem } from '@/shared/ui'
import type { AdminModelOption } from '../model/aiAdapter'
import { buildModelPickerOptions, type ModelPickerOption } from '../model/modelPicker'
import { OPENAI_COMPATIBLE_RUNTIME_KIND } from '../model/reasoningCapability'
import { ModelPickerField } from './ModelPickerField'
import { ReasoningField } from './ReasoningField'
import {
  BINDING_PURPOSES,
  isDecisionBindingPurpose,
  providerServesPurpose,
  purposeDescription,
  purposeLabel,
  resolveBindingForPurpose,
  suggestBindingSelection,
  type AiScopeContext,
  type BindingResolution,
} from '../model/aiConfig'
import { useAiAdminMutations, useAvailableProviderModels, useBindingDetail } from './api'
import { BindingRoute, ValidationHistory } from './BindingSupport'
import {
  EMPTY_BINDING_FORM,
  REASONING_EFFORTS,
  SERVICE_TIERS,
  applyManagedReasoningMode,
  deriveReasoningJournal,
  formFromBinding,
  requestTuningValue,
  withRequestTuning,
  withoutExtraParameter,
  withoutManagedReasoningMode,
  type BindingForm,
  type ManagedReasoningMode,
} from './bindingForm'
import { aiScopeLabel } from './labels'
import { errorText, parseJsonObject, scopeBody } from './utils'

/**
 * What a model costs when the provider publishes no rates. Zero is what the
 * catalog stores, so the dialog warns before the operator commits rather than
 * letting a paid model report as free.
 */
const ZERO_PRICING = {
  currencyCode: 'USD',
  inputPricePer1m: '0',
  cachedInputPricePer1m: '0',
  cacheWriteInputPricePer1m: '0',
  outputPricePer1m: '0',
} as const

export function BindingsSection({
  selectedScope,
  scopeContext,
  availableAccounts,
  models,
  bindingsForScope,
  instanceBindings,
  workspaceBindings,
}: Readonly<{
  selectedScope: AIScopeKind
  scopeContext: AiScopeContext
  availableAccounts: AIAccount[]
  models: AdminModelOption[]
  bindingsForScope: AIBinding[]
  instanceBindings: AIBinding[]
  workspaceBindings: AIBinding[]
}>) {
  const { t } = useTranslation()
  const mutations = useAiAdminMutations()
  const formId = useId()
  const [editingPurpose, setEditingPurpose] = useState<AiBindingPurpose | null>(null)
  const [editingBindingId, setEditingBindingId] = useState<string | null>(null)
  const [form, setForm] = useState<BindingForm>(EMPTY_BINDING_FORM)
  const [error, setError] = useState('')
  const detail = useBindingDetail(editingBindingId)
  const resolutionByPurpose = useMemo(
    () =>
      new Map(
        BINDING_PURPOSES.map((purpose) => [
          purpose,
          resolveBindingForPurpose({
            purpose,
            selectedScope,
            bindingsForScope,
            instanceBindings,
            workspaceBindings,
          }),
        ]),
      ),
    [bindingsForScope, instanceBindings, selectedScope, workspaceBindings],
  )
  const resolution = editingPurpose ? resolutionByPurpose.get(editingPurpose) : undefined
  const selectedAccount = availableAccounts.find((account) => account.id === form.accountId)
  // Two sources, one list. The catalog holds what this build seeded and what an
  // operator added; the provider knows what the key actually reaches right now.
  // A router seeds no chat models at all, so without the second source its
  // dropdown was empty with nothing to explain it.
  const availableModels = useAvailableProviderModels(
    editingPurpose && selectedAccount ? selectedAccount.id : null,
  )
  const providerModels = models.filter(
    (model) => !selectedAccount || model.providerCatalogId === selectedAccount.providerId,
  )
  const options = useMemo(
    () =>
      buildModelPickerOptions({
        purpose: editingPurpose,
        catalog: providerModels,
        // Every column the picker shows comes from here. Mapping only the name
        // and the modalities left the table with a dash in every cell while the
        // API was returning context windows and rates for four hundred models.
        discovered: (availableModels.data ?? []).map((model) => ({
          modelName: model.modelName,
          inputModalities: model.inputModalities,
          contextWindow: model.contextWindow,
          maxOutputTokens: model.maxOutputTokens,
          pricing: model.pricing
            ? {
                currencyCode: model.pricing.currencyCode,
                inputPricePer1m: model.pricing.inputPricePer1m,
                outputPricePer1m: model.pricing.outputPricePer1m,
              }
            : null,
        })),
      }),
    // `providerModels` is derived per render; its identity changes every time,
    // so the list it was derived from is what the memo keys on.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [editingPurpose, models, selectedAccount?.providerId, availableModels.data],
  )
  const selectableOptions = options.filter((option) => option.selectable)
  const selectedOption = options.find((option) => option.key === form.modelId)
  const selectedModel = models.find((model) => model.id === selectedOption?.modelCatalogId)
  const selectedUnavailable = Boolean(
    selectedAccount &&
    selectedModel &&
    !selectedModel.availableAccountIds.includes(selectedAccount.id),
  )
  // The typed reasoning selection is capability-aware: the exact model's catalog
  // confirmation decides whether a mode may be offered, and a non-OpenAI
  // runtime has no confirmed wire adapter, so nothing is selectable there. The
  // decision table itself lives in `deriveReasoningJournal`, where it is one
  // pure function instead of a spread of branches across this render.
  const reasoning = deriveReasoningJournal({
    purpose: editingPurpose,
    hasSelectedAccount: Boolean(selectedAccount),
    capability: selectedModel?.reasoningCapability,
    runtimeCapable: selectedAccount?.provider?.runtime?.kind === OPENAI_COMPATIBLE_RUNTIME_KIND,
    form,
  })

  const close = () => {
    setEditingPurpose(null)
    setEditingBindingId(null)
    setForm(EMPTY_BINDING_FORM)
    setError('')
  }
  const open = (purpose: AiBindingPurpose) => {
    const resolved = resolutionByPurpose.get(purpose)
    setEditingPurpose(purpose)
    setEditingBindingId(resolved?.localBinding?.id ?? null)
    if (resolved?.localBinding) setForm(formFromBinding(resolved.localBinding))
    else {
      const suggestion = suggestBindingSelection({
        purpose,
        availableAccounts,
        models,
        preferredAccountId: resolved?.effectiveBinding?.accountId,
        preferredModelCatalogId: resolved?.effectiveBinding?.modelCatalogId,
      })
      // The suggestion names a catalog row, and a catalog row's option key is
      // its id, so this lands on the same entry the dropdown will show.
      setForm({
        ...EMPTY_BINDING_FORM,
        accountId: suggestion.accountId,
        modelId: suggestion.modelCatalogId,
      })
    }
  }
  /**
   * The catalog id for whatever the operator picked, creating or extending the
   * row when the pick came from the provider's live listing rather than the
   * catalog. A binding can only point at a catalog row, so this is the step
   * that turns "the provider offers this" into something bindable.
   *
   * Fields come from what the provider published: absent means absent. The one
   * exception is the price, which the catalog cannot leave unset — a router
   * that publishes no rates yields zeros, and the dialog says so before the
   * operator commits.
   */
  const catalogIdForSelection = async (option: ModelPickerOption): Promise<string> => {
    if (!editingPurpose) throw new Error('a binding is always edited for a purpose')
    if (!option.modelCatalogId) {
      const listed = (availableModels.data ?? []).find(
        (model) => model.modelName === option.modelName,
      )
      const created = await mutations.modelCreate.mutateAsync({
        providerCatalogId: selectedAccount?.providerId ?? '',
        modelName: option.modelName,
        lifecycleState: 'active',
        allowedBindingPurposes: [editingPurpose],
        contextWindow: listed?.contextWindow ?? null,
        maxOutputTokens: listed?.maxOutputTokens ?? null,
        metadataJson: { observedInputModalities: listed?.inputModalities ?? [] },
        pricing: listed?.pricing ?? ZERO_PRICING,
      })
      return created.id
    }
    if (option.note === 'adds_purpose') {
      const model = models.find((entry) => entry.id === option.modelCatalogId)
      if (model) {
        await mutations.modelUpdate.mutateAsync({
          id: model.id,
          body: {
            lifecycleState: model.lifecycleState ?? 'active',
            allowedBindingPurposes: [...model.allowedBindingPurposes, editingPurpose],
            contextWindow: model.contextWindow ?? null,
            maxOutputTokens: model.maxOutputTokens ?? null,
            pricing: model.pricing,
          },
        })
      }
    }
    return option.modelCatalogId
  }
  const bodyFromForm = (modelCatalogId: string) => {
    const extra = applyManagedReasoningMode(
      parseJsonObject(form.extraJson) ?? {},
      form.reasoningMode,
    )
    return {
      accountId: form.accountId,
      modelCatalogId,
      bindingState: form.state,
      systemPrompt: form.systemPrompt.trim() || undefined,
      temperature: form.temperature ? Number(form.temperature) : undefined,
      topP: form.topP ? Number(form.topP) : undefined,
      maxOutputTokensOverride: form.maxOutputTokens ? Number(form.maxOutputTokens) : undefined,
      ...(Object.keys(extra).length > 0 ? { extraParametersJson: extra } : {}),
    }
  }
  const save = async (event: FormEvent) => {
    event.preventDefault()
    if (!editingPurpose || selectedUnavailable || !selectedOption || reasoning.blocked) return
    setError('')
    try {
      const body = bodyFromForm(await catalogIdForSelection(selectedOption))
      if (resolution?.localBinding)
        await mutations.bindingUpdate.mutateAsync({ id: resolution.localBinding.id, body })
      else
        await mutations.bindingCreate.mutateAsync({
          ...scopeBody(selectedScope, scopeContext),
          bindingPurpose: editingPurpose,
          ...body,
        })
      toast.success(t('admin.aiPanel.messages.bindingSaved'))
      close()
    } catch (saveError) {
      const problem = getApiProblem(saveError)
      const reasoningPresent =
        form.reasoningMode !== '' ||
        form.reasoningStored.kind !== 'unset' ||
        reasoning.managedKeyTyped ||
        reasoning.rawConflict !== null
      // The server's reason is authoritative but internal prose; a reasoning
      // write the operator can act on gets a localized explanation instead.
      if (reasoningPresent && problem && problem.status >= 400 && problem.status < 500) {
        setError(t('admin.aiPanel.reasoning.saveRejected'))
        return
      }
      setError(errorText(saveError, t('admin.aiPanel.messages.bindingSaveFailed'), t))
    }
  }
  const unsetStoredReasoning = () =>
    setForm((current) => ({ ...current, reasoningStored: { kind: 'unset' } }))
  const setPendingReasoning = (mode: ManagedReasoningMode) =>
    setForm((current) => ({
      ...current,
      reasoningMode: mode,
      reasoningStored: { kind: 'unset' },
      reasoningTarget:
        mode === '' ? null : { accountId: current.accountId, modelId: current.modelId },
    }))
  const unsetPendingReasoning = () => setPendingReasoning('')
  const removeConflictingParameter = (parameter: string) =>
    setForm((current) => ({
      ...current,
      extraJson: withoutExtraParameter(current.extraJson, parameter),
    }))
  const removeManagedKeyFromRaw = () =>
    setForm((current) => ({
      ...current,
      extraJson: withoutManagedReasoningMode(current.extraJson).extraJson,
    }))
  const reset = async (binding: AIBinding) => {
    if (
      !window.confirm(
        t('admin.aiPanel.dialogs.resetBindingDescription', {
          purpose: purposeLabel(binding.purpose, t),
        }),
      )
    ) {
      return
    }
    try {
      await mutations.bindingDelete.mutateAsync(binding.id)
      toast.success(t('admin.aiPanel.messages.overrideRemoved'))
    } catch (deleteError) {
      toast.error(errorText(deleteError, t('admin.aiPanel.messages.overrideRemoveFailed'), t))
    }
  }
  const toggle = async (binding: AIBinding) => {
    const body = formFromBinding(binding)
    await mutations.bindingUpdate.mutateAsync({
      id: binding.id,
      body: {
        accountId: body.accountId,
        modelCatalogId: body.modelId,
        bindingState: binding.state === 'active' ? 'disabled' : 'active',
        systemPrompt: binding.systemPrompt,
        temperature: binding.temperature,
        topP: binding.topP,
        maxOutputTokensOverride: binding.maxOutputTokens,
        extraParametersJson: binding.extraParams,
      },
    })
  }

  return (
    <>
      <Panel>
        <SettingsList label={t('admin.bindings')}>
          {BINDING_PURPOSES.map((purpose) => {
            const resolved = resolutionByPurpose.get(purpose) as BindingResolution
            const binding = resolved.effectiveBinding
            return (
              <SettingsListItem
                key={purpose}
                title={purposeLabel(purpose, t)}
                description={
                  <>
                    {binding?.state !== 'active' && (
                      <Badge tone="warning">
                        {bindingStateLabel(binding?.state ?? 'missing', t)}
                      </Badge>
                    )}
                    {resolved.sourceKind && (
                      <span>
                        {resolved.localBinding
                          ? t('admin.aiPanel.localBinding')
                          : t('admin.aiPanel.inheritedBinding', {
                              scope: aiScopeLabel(resolved.sourceKind, t),
                            })}
                      </span>
                    )}
                  </>
                }
                summary={
                  <BindingRoute
                    resolved={resolved}
                    accounts={availableAccounts}
                    models={models}
                    compact
                  />
                }
                actions={
                  <Button size="sm" variant="ghost" onClick={() => open(purpose)}>
                    {binding ? t('common.edit') : t('admin.aiPanel.actions.setDefault')}
                  </Button>
                }
              >
                <p>{purposeDescription(purpose, t)}</p>
                {resolved.localBinding && (
                  <div className="binding-settings">
                    <label className="switch">
                      <input
                        type="checkbox"
                        checked={resolved.localBinding.state === 'active'}
                        disabled={mutations.bindingUpdate.isPending}
                        onChange={() => void toggle(resolved.localBinding as AIBinding)}
                      />
                      <span />
                      {t('admin.aiPanel.actions.toggleBinding')}
                    </label>
                    {selectedScope !== 'instance' && (
                      <Button
                        size="sm"
                        variant="ghost"
                        disabled={mutations.bindingDelete.isPending}
                        onClick={() => void reset(resolved.localBinding as AIBinding)}
                      >
                        {t('admin.aiPanel.actions.resetInheritance')}
                      </Button>
                    )}
                  </div>
                )}
              </SettingsListItem>
            )
          })}
        </SettingsList>
      </Panel>
      <Modal
        open={Boolean(editingPurpose)}
        onOpenChange={(next) => !next && close()}
        title={`${resolution?.localBinding ? t('common.edit') : t('admin.aiPanel.actions.override')} · ${editingPurpose ? purposeLabel(editingPurpose, t) : ''}`}
        description={
          editingPurpose ? purposeDescription(editingPurpose, t) : t('admin.inheritance')
        }
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
              disabled={
                selectedUnavailable ||
                reasoning.blocked ||
                mutations.bindingCreate.isPending ||
                mutations.bindingUpdate.isPending
              }
            >
              {t('admin.save')}
            </Button>
          </>
        }
      >
        <form
          id={formId}
          className="ai-form ai-binding-form"
          onSubmit={(event) => void save(event)}
        >
          <div className="ai-binding-context">
            <Badge>{aiScopeLabel(selectedScope, t)}</Badge>
            {resolution?.sourceKind && resolution.sourceKind !== selectedScope && (
              <span>
                {t('admin.aiPanel.inheritedFrom', {
                  scope: aiScopeLabel(resolution.sourceKind, t),
                })}
              </span>
            )}
            {resolution && (
              <BindingRoute resolved={resolution} accounts={availableAccounts} models={models} />
            )}
          </div>
          {editingBindingId && detail.isPending ? (
            <div className="ai-empty">{t('admin.loading')}</div>
          ) : (
            <div className="field-grid">
              <label className="field field--wide">
                <span>{t('admin.aiPanel.fields.account')}</span>
                <select
                  className="input"
                  required
                  value={form.accountId}
                  onChange={(event) => {
                    const account = availableAccounts.find(
                      (entry) => entry.id === event.target.value,
                    )
                    const decisionOnly = account?.provider?.capabilities?.decision === 'supported'
                    const accountId = event.target.value
                    setForm((current) => ({
                      ...current,
                      accountId,
                      modelId: '',
                      reasoningTarget: null,
                      ...(decisionOnly ? { state: 'disabled' } : {}),
                    }))
                  }}
                >
                  <option value="">{t('admin.aiPanel.placeholders.selectAccount')}</option>
                  {availableAccounts
                    .filter(
                      (account) =>
                        account.state === 'active' &&
                        providerServesPurpose(account.provider, editingPurpose),
                    )
                    .map((account) => (
                      <option key={account.id} value={account.id}>
                        {account.label} · {aiScopeLabel(account.scopeKind, t)}
                      </option>
                    ))}
                </select>
                {selectedAccount?.provider?.capabilities?.decision === 'supported' &&
                  editingPurpose !== null &&
                  isDecisionBindingPurpose(editingPurpose) && (
                    <small>{t('admin.aiPanel.messages.decisionBindingNotice')}</small>
                  )}
              </label>
              <div className="field field--wide">
                <span>{t('admin.model')}</span>
                {selectedAccount ? (
                  <ModelPickerField
                    key={selectedAccount.id}
                    options={options}
                    value={form.modelId}
                    loading={availableModels.isFetching}
                    onChange={(key) =>
                      setForm((current) =>
                        current.accountId !== selectedAccount.id
                          ? current
                          : {
                              ...current,
                              modelId: key,
                              reasoningTarget:
                                current.modelId === key ? current.reasoningTarget : null,
                            },
                      )
                    }
                  />
                ) : (
                  <p className="ai-empty">{t('admin.aiPanel.placeholders.selectAccount')}</p>
                )}
                {/* The picker is not a form control, so the browser cannot
                    require it. Without this a dialog could be submitted with
                    nothing chosen and fail at the API instead of on screen. */}
                <input
                  type="text"
                  className="model-picker__requirement"
                  required
                  tabIndex={-1}
                  aria-hidden="true"
                  value={form.modelId}
                  onChange={() => undefined}
                />
                <ModelFieldHints
                  providerModelsFailed={availableModels.isError}
                  providerModelsLoading={availableModels.isFetching}
                  optionCount={options.length}
                  selectableCount={selectableOptions.length}
                  selectedNote={selectedOption?.note}
                  selectedPriceMissing={!selectedOption?.price}
                  selectedUnavailable={selectedUnavailable}
                />
              </div>
              <details
                className="ai-binding-parameters field--wide"
                open={reasoning.blocked || undefined}
              >
                <summary>{t('admin.aiWizard.steps.model.title')}</summary>
                <div className="field-grid">
                  <label className="field">
                    <span>{t('admin.temperature')}</span>
                    <input
                      className="input"
                      type="number"
                      step="0.1"
                      value={form.temperature}
                      onChange={(event) => setForm({ ...form, temperature: event.target.value })}
                    />
                  </label>
                  <label className="field">
                    <span>{t('admin.topP')}</span>
                    <input
                      className="input"
                      type="number"
                      step="0.1"
                      value={form.topP}
                      onChange={(event) => setForm({ ...form, topP: event.target.value })}
                    />
                  </label>
                  <label className="field">
                    <span>{t('admin.maxOutputTokens')}</span>
                    <input
                      className="input"
                      type="number"
                      min="0"
                      value={form.maxOutputTokens}
                      onChange={(event) =>
                        setForm({ ...form, maxOutputTokens: event.target.value })
                      }
                    />
                  </label>
                  {reasoning.show && (
                    <ReasoningField
                      formId={formId}
                      journal={reasoning}
                      mode={form.reasoningMode}
                      onSelectMode={setPendingReasoning}
                      onConfirm={() => setPendingReasoning(form.reasoningMode)}
                      onUnsetStored={unsetStoredReasoning}
                      onUnsetPending={unsetPendingReasoning}
                      onRemoveConflict={removeConflictingParameter}
                      onRemoveManagedKey={removeManagedKeyFromRaw}
                    />
                  )}
                  {editingPurpose !== 'embed_chunk' &&
                    selectedAccount?.provider?.capabilities?.chat === 'supported' && (
                      <label className="field">
                        <span>{t('admin.aiPanel.fields.reasoningEffort')}</span>
                        <select
                          className="input"
                          value={requestTuningValue(form.extraJson, 'reasoningEffort') ?? ''}
                          disabled={requestTuningValue(form.extraJson, 'reasoningEffort') === null}
                          onChange={(event) =>
                            setForm({
                              ...form,
                              extraJson: withRequestTuning(
                                form.extraJson,
                                'reasoningEffort',
                                event.target.value,
                              ),
                            })
                          }
                        >
                          <option value="">
                            {t('admin.aiPanel.requestTuning.providerDefault')}
                          </option>
                          {REASONING_EFFORTS.map((effort) => (
                            <option key={effort} value={effort}>
                              {t(`admin.aiPanel.requestTuning.reasoning.${effort}`)}
                            </option>
                          ))}
                        </select>
                        <small className="field-hint">
                          {t('admin.aiPanel.requestTuning.reasoningHint')}
                        </small>
                      </label>
                    )}
                  {selectedAccount?.provider?.capabilities?.flexTier === 'supported' && (
                    <label className="field">
                      <span>{t('admin.aiPanel.fields.serviceTier')}</span>
                      <select
                        className="input"
                        value={requestTuningValue(form.extraJson, 'serviceTier') ?? ''}
                        disabled={requestTuningValue(form.extraJson, 'serviceTier') === null}
                        onChange={(event) =>
                          setForm({
                            ...form,
                            extraJson: withRequestTuning(
                              form.extraJson,
                              'serviceTier',
                              event.target.value,
                            ),
                          })
                        }
                      >
                        <option value="">{t('admin.aiPanel.requestTuning.tier.standard')}</option>
                        {SERVICE_TIERS.map((tier) => (
                          <option key={tier} value={tier}>
                            {t(`admin.aiPanel.requestTuning.tier.${tier}`)}
                          </option>
                        ))}
                      </select>
                      <small className="field-hint">
                        {t('admin.aiPanel.requestTuning.tierHint')}
                      </small>
                    </label>
                  )}
                  <label className="field">
                    <span>{t('admin.status')}</span>
                    <select
                      className="input"
                      value={form.state}
                      onChange={(event) =>
                        setForm({ ...form, state: event.target.value as AIBindingState })
                      }
                    >
                      {(['active', 'disabled'] as const).map((state) => (
                        <option key={state} value={state}>
                          {bindingStateLabel(state, t)}
                        </option>
                      ))}
                    </select>
                  </label>
                  <label className="field field--wide">
                    <span>{t('admin.aiPanel.fields.systemPrompt')}</span>
                    <textarea
                      className="input"
                      value={form.systemPrompt}
                      onChange={(event) => setForm({ ...form, systemPrompt: event.target.value })}
                    />
                  </label>
                  <label className="field field--wide">
                    <span>{t('admin.aiPanel.fields.extraParametersJson')}</span>
                    <textarea
                      className="input code-input"
                      value={form.extraJson}
                      onChange={(event) => setForm({ ...form, extraJson: event.target.value })}
                    />
                    <small className="field-hint">{t('admin.aiPanel.notices.promptCache')}</small>
                  </label>
                </div>
              </details>
            </div>
          )}
          {error && <p className="ai-inline-error">{error}</p>}
          {resolution?.localBinding && <ValidationHistory bindingId={resolution.localBinding.id} />}
        </form>
      </Modal>
    </>
  )
}

/**
 * Why the model picker cannot offer what the operator needs: provider listing
 * failure, empty catalog, nothing selectable for this purpose, an entry that
 * will be created on save, or an unavailable selection. Every line names one
 * condition, so the dialog never relies on the operator inferring it.
 */
function ModelFieldHints({
  providerModelsFailed,
  providerModelsLoading,
  optionCount,
  selectableCount,
  selectedNote,
  selectedPriceMissing,
  selectedUnavailable,
}: Readonly<{
  providerModelsFailed: boolean
  providerModelsLoading: boolean
  optionCount: number
  selectableCount: number
  selectedNote: ModelPickerOption['note'] | undefined
  selectedPriceMissing: boolean
  selectedUnavailable: boolean
}>) {
  const { t } = useTranslation()
  return (
    <>
      {providerModelsFailed && (
        <small>{t('admin.aiPanel.messages.providerModelsUnavailable')}</small>
      )}
      {optionCount === 0 && !providerModelsLoading && (
        <small>{t('admin.aiPanel.messages.providerHasNoModels')}</small>
      )}
      {optionCount > 0 && selectableCount === 0 && (
        <small>{t('admin.aiPanel.messages.noModelServesPurpose')}</small>
      )}
      {selectedNote === 'not_in_catalog' && (
        <small>{t('admin.aiPanel.messages.modelWillBeAdded')}</small>
      )}
      {selectedNote === 'not_in_catalog' && selectedPriceMissing && (
        <small>{t('admin.aiPanel.messages.modelHasNoPublishedPrice')}</small>
      )}
      {selectedUnavailable && <small>{t('admin.aiPanel.messages.selectedModelUnavailable')}</small>}
    </>
  )
}
