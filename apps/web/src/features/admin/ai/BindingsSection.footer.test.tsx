import { fireEvent, render, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import type { ComponentProps } from 'react'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import i18n from '@/shared/i18n'
import { ApiError } from '@/shared/api'
import type { AIAccount, AIBinding, AIProvider } from '@/shared/types'
import type { AdminModelOption } from '../model/aiAdapter'
import { BindingsSection } from './BindingsSection'

const createMutation = () => ({ isPending: false, mutateAsync: vi.fn() })
const mutations = {
  modelCreate: createMutation(),
  modelUpdate: createMutation(),
  bindingCreate: createMutation(),
  bindingUpdate: createMutation(),
  bindingDelete: createMutation(),
  bindingValidate: createMutation(),
}

vi.mock('./api', () => ({
  useAiAdminMutations: () => mutations,
  useAvailableProviderModels: () => ({ data: [], isFetching: false, isError: false }),
  useBindingDetail: () => ({ data: undefined, isPending: false }),
  useBindingValidations: () => ({ data: [], isPending: false }),
  useBindingValidation: () => ({ data: undefined, isPending: false }),
}))

vi.mock('@/shared/contexts/app-context', () => ({
  useApp: () => ({ locale: 'en' }),
}))

const provider: AIProvider = {
  id: 'provider-1',
  displayName: 'Provider One',
  purposeGaps: [],
  kind: 'openai-compatible',
  apiStyle: 'chat-completions',
  lifecycleState: 'active',
  defaultBaseUrl: 'https://provider.example/v1',
  apiKeyRequired: true,
  baseUrlRequired: false,
  credentialPolicy: {
    apiKeyRequired: true,
    baseUrlRequired: false,
    baseUrlMode: 'fixed',
    validationMode: 'model_list',
  },
  baseUrlPolicy: {
    allowOverride: false,
    requireHttps: true,
    allowPrivateNetwork: false,
    trimSuffixes: [],
  },
  modelDiscovery: { mode: 'shared', paths: [] },
  capabilities: {},
  runtime: {},
  uiHints: {},
  modelCount: 1,
  credentialCount: 1,
}

const account: AIAccount = {
  id: 'account-1',
  scopeKind: 'instance',
  providerId: provider.id,
  providerName: provider.displayName,
  providerKind: provider.kind,
  provider,
  label: 'Instance account',
  state: 'active',
  createdAt: '2026-01-01T00:00:00Z',
  updatedAt: '2026-01-01T00:00:00Z',
  apiKeySummary: 'configured',
}

const model: AdminModelOption = {
  id: 'model-1',
  providerCatalogId: provider.id,
  modelName: 'Model One',
  allowedBindingPurposes: ['query_answer'],
  availabilityState: 'available',
  availableAccountIds: [account.id],
  reasoningCapability: { state: 'unconfirmed' },
  seeded: true,
  pricing: {
    cacheWriteInputPricePer1m: '0',
    cachedInputPricePer1m: '0',
    currencyCode: 'USD',
    inputPricePer1m: '0.20',
    outputPricePer1m: '1.20',
  },
}

const instanceBinding: AIBinding = {
  id: 'binding-instance-1',
  scopeKind: 'instance',
  purpose: 'query_answer',
  accountId: account.id,
  modelCatalogId: model.id,
  state: 'active',
}

function renderSection(overrides: Partial<ComponentProps<typeof BindingsSection>> = {}) {
  return render(
    <BindingsSection
      selectedScope="library"
      scopeContext={{ workspaceId: 'workspace-1', libraryId: 'library-1' }}
      availableAccounts={[account]}
      models={[model]}
      bindingsForScope={[]}
      instanceBindings={[instanceBinding]}
      workspaceBindings={[]}
      {...overrides}
    />,
  )
}

describe('BindingsSection override dialog', () => {
  beforeEach(async () => {
    vi.clearAllMocks()
    await i18n.changeLanguage('en')
  })

  it('puts the dialog actions in the shared footer, outside the scrolling body', async () => {
    const user = userEvent.setup()
    renderSection()

    const openers = screen.getAllByRole('button', { name: 'Set up' })
    await user.click(openers[0] as HTMLElement)

    const dialog = await screen.findByRole('dialog')
    const footer = dialog.querySelector<HTMLElement>('.dialog-footer')
    const body = dialog.querySelector<HTMLElement>('.dialog-body')
    expect(footer).not.toBeNull()
    expect(body).not.toBeNull()

    const save = within(dialog).getByRole('button', { name: 'Save' })
    expect(footer).toContainElement(save)
    expect(body).not.toContainElement(save)
    expect(dialog.querySelector('form')).not.toBeNull()
  })

  it('separates the inherited route from the new selection and keeps tuning behind a disclosure', async () => {
    const user = userEvent.setup()
    renderSection()
    const row = screen.getByRole('listitem', { name: 'Answer Generation' })
    await user.click(within(row).getByRole('button', { name: 'Edit' }))
    const dialog = await screen.findByRole('dialog')
    const context = dialog.querySelector('.ai-binding-context') as HTMLElement
    expect(context).toHaveTextContent('Library')
    expect(context).toHaveTextContent('Inherited from level: Instance')
    expect(context).toHaveTextContent('Instance account')
    expect(context).toHaveTextContent('Model One')
    expect(within(dialog).getByRole('status')).toHaveTextContent('Model One')
    const parameters = dialog.querySelector('details') as HTMLDetailsElement
    expect(parameters.open).toBe(false)
    await user.click(within(dialog).getByText('Model and parameters'))
    expect(parameters.open).toBe(true)
    expect(within(dialog).getByLabelText('Temperature')).toBeVisible()
  })

  it('saves the inherited suggestion as a library override without expanding tuning', async () => {
    const user = userEvent.setup()
    renderSection()
    const row = screen.getByRole('listitem', { name: 'Answer Generation' })
    await user.click(within(row).getByRole('button', { name: 'Edit' }))
    const dialog = await screen.findByRole('dialog')
    await user.click(within(dialog).getByRole('button', { name: 'Save' }))
    await waitFor(() =>
      expect(mutations.bindingCreate.mutateAsync).toHaveBeenCalledWith({
        scopeKind: 'library',
        workspaceId: 'workspace-1',
        libraryId: 'library-1',
        bindingPurpose: 'query_answer',
        accountId: account.id,
        modelCatalogId: model.id,
        bindingState: 'active',
        systemPrompt: undefined,
        temperature: undefined,
        topP: undefined,
        maxOutputTokensOverride: undefined,
      }),
    )
    expect(mutations.bindingUpdate.mutateAsync).not.toHaveBeenCalled()
    expect(mutations.modelCreate.mutateAsync).not.toHaveBeenCalled()
  })

  it('requires an account before showing model choices and resets selection when it changes', async () => {
    const user = userEvent.setup()
    renderSection()
    const row = screen.getByRole('listitem', { name: 'Answer Generation' })
    await user.click(within(row).getByRole('button', { name: 'Edit' }))
    const dialog = await screen.findByRole('dialog')
    await user.selectOptions(within(dialog).getByLabelText('Account'), '')
    expect(within(dialog).queryByRole('table')).not.toBeInTheDocument()
    expect(within(dialog).getByText('Select account', { selector: 'p' })).toBeVisible()
    await user.selectOptions(within(dialog).getByLabelText('Account'), account.id)
    expect(within(dialog).getByRole('status')).not.toHaveTextContent('Model One')
    expect(within(dialog).getByRole('table')).toBeVisible()
  })

  it('shows a compact inherited summary and reveals the purpose explanation on demand', async () => {
    const user = userEvent.setup()
    renderSection()
    expect(screen.getAllByRole('listitem')).toHaveLength(8)
    const row = screen.getByRole('listitem', { name: 'Answer Generation' })
    expect(row).toHaveTextContent('Inherited · Instance')
    expect(row).toHaveTextContent('Model One')
    expect(row).toHaveTextContent('Instance account')
    expect(within(row).queryByText('Active')).not.toBeInTheDocument()
    expect(within(row).queryByText('Account', { exact: true })).not.toBeInTheDocument()
    expect(within(row).getAllByRole('button')).toHaveLength(2)
    const description = within(row).getByText(
      'Generate answers grounded in retrieved library evidence.',
    )
    expect(description).not.toBeVisible()
    await user.click(within(row).getByRole('button', { name: 'Answer Generation' }))
    expect(description).toBeVisible()
  })

  it('distinguishes a missing binding from an unavailable inherited model', () => {
    renderSection({ models: [{ ...model, availableAccountIds: [] }] })
    const missing = screen.getByRole('listitem', { name: 'Document Understanding' })
    expect(missing).toHaveTextContent('Not configured')
    expect(within(missing).getByRole('button', { name: 'Set up' })).toBeVisible()
    const unavailable = screen.getByRole('listitem', { name: 'Answer Generation' })
    expect(unavailable).toHaveTextContent('Unavailable')
    expect(unavailable).toHaveTextContent('Inherited · Instance')
  })

  it('keeps local enable and inheritance reset actions inside the purpose details', async () => {
    const user = userEvent.setup()
    const local = {
      ...instanceBinding,
      id: 'local-1',
      scopeKind: 'library',
      state: 'disabled',
    } as const
    renderSection({ bindingsForScope: [local] })
    const row = screen.getByRole('listitem', { name: 'Answer Generation' })
    expect(row).toHaveTextContent('Custom setting')
    expect(row).toHaveTextContent('Disabled')
    expect(within(row).getByRole('checkbox', { hidden: true })).not.toBeVisible()
    await user.click(within(row).getByRole('button', { name: 'Answer Generation' }))
    await user.click(within(row).getByRole('checkbox', { name: 'Enable this binding' }))
    expect(mutations.bindingUpdate.mutateAsync).toHaveBeenCalledWith({
      id: local.id,
      body: expect.objectContaining({
        accountId: account.id,
        modelCatalogId: model.id,
        bindingState: 'active',
      }),
    })
    const confirm = vi.spyOn(window, 'confirm').mockReturnValue(false)
    await user.click(within(row).getByRole('button', { name: 'Use inherited settings' }))
    expect(mutations.bindingDelete.mutateAsync).not.toHaveBeenCalled()
    expect(confirm).toHaveBeenCalledWith(
      'Remove the custom setting for Answer Generation and use the inherited model and parameters?',
    )
    confirm.mockReturnValue(true)
    await user.click(within(row).getByRole('button', { name: 'Use inherited settings' }))
    expect(mutations.bindingDelete.mutateAsync).toHaveBeenCalledWith(local.id)
    confirm.mockRestore()
  })

  it('does not offer inheritance reset for an instance binding', async () => {
    const user = userEvent.setup()
    renderSection({ selectedScope: 'instance', bindingsForScope: [instanceBinding] })
    const row = screen.getByRole('listitem', { name: 'Answer Generation' })
    await user.click(within(row).getByRole('button', { name: 'Answer Generation' }))
    expect(within(row).getByRole('checkbox', { name: 'Enable this binding' })).toBeChecked()
    expect(
      within(row).queryByRole('button', { name: 'Use inherited settings' }),
    ).not.toBeInTheDocument()
  })
})

const EVIDENCE = 'https://developers.openai.com/api/docs/guides/reasoning'

const chatProvider: AIProvider = {
  ...provider,
  capabilities: { chat: 'supported' },
  runtime: { kind: 'openai_compatible' },
}

const chatAccount: AIAccount = { ...account, provider: chatProvider }

const confirmedModel: AdminModelOption = {
  ...model,
  reasoningCapability: {
    state: 'confirmed',
    mode: 'none',
    wire: 'reasoning_effort_none',
    evidence: EVIDENCE,
  },
}

const secondModel: AdminModelOption = {
  ...model,
  id: 'model-2',
  modelName: 'Model Two',
  reasoningCapability: { state: 'unconfirmed' },
}

function libraryBinding(extraParams: Record<string, unknown>): AIBinding {
  return { ...instanceBinding, id: 'local-1', scopeKind: 'library', extraParams }
}

async function openAnswerDialog(user: ReturnType<typeof userEvent.setup>) {
  const row = screen.getByRole('listitem', { name: 'Answer Generation' })
  await user.click(within(row).getByRole('button', { name: 'Edit' }))
  const dialog = await screen.findByRole('dialog')
  // The reasoning, raw-JSON and tuning controls live behind the "Model and
  // parameters" disclosure; open it so the controls are visible.
  if (!(dialog.querySelector('details') as HTMLDetailsElement).open) {
    await user.click(within(dialog).getByText('Model and parameters'))
  }
  return dialog
}

describe('BindingsSection reasoning selector', () => {
  beforeEach(async () => {
    vi.clearAllMocks()
    await i18n.changeLanguage('en')
  })

  it('round-trips a saved mode and keeps the managed key out of the raw JSON', async () => {
    const user = userEvent.setup()
    renderSection({
      models: [confirmedModel],
      availableAccounts: [chatAccount],
      bindingsForScope: [libraryBinding({ seed: 7, reasoningMode: { version: 1, mode: 'none' } })],
    })
    const dialog = await openAnswerDialog(user)

    expect(within(dialog).getByLabelText('Reasoning mode')).toHaveValue('none')
    const raw = within(dialog).getByLabelText<HTMLTextAreaElement>(/Additional parameters/)
    expect(raw.value).toContain('"seed": 7')
    expect(raw.value).not.toContain('reasoningMode')

    await user.click(within(dialog).getByRole('button', { name: 'Save' }))
    await waitFor(() =>
      expect(mutations.bindingUpdate.mutateAsync).toHaveBeenCalledWith({
        id: 'local-1',
        body: expect.objectContaining({
          extraParametersJson: { seed: 7, reasoningMode: { version: 1, mode: 'none' } },
        }),
      }),
    )
  })

  it('saves an explicit Off selection as the typed envelope', async () => {
    const user = userEvent.setup()
    renderSection({ models: [confirmedModel], availableAccounts: [chatAccount] })
    const dialog = await openAnswerDialog(user)

    await user.selectOptions(within(dialog).getByLabelText('Reasoning mode'), 'none')
    await user.click(within(dialog).getByRole('button', { name: 'Save' }))
    await waitFor(() =>
      expect(mutations.bindingCreate.mutateAsync).toHaveBeenCalledWith(
        expect.objectContaining({
          bindingPurpose: 'query_answer',
          extraParametersJson: { reasoningMode: { version: 1, mode: 'none' } },
        }),
      ),
    )
  })

  it('reopens the mode returned by a save rather than the original binding snapshot', async () => {
    const user = userEvent.setup()
    let persisted = libraryBinding({ vendor_extension: { keep: 7 } })
    const props: ComponentProps<typeof BindingsSection> = {
      selectedScope: 'library',
      scopeContext: { workspaceId: 'workspace-1', libraryId: 'library-1' },
      availableAccounts: [chatAccount],
      models: [confirmedModel],
      bindingsForScope: [persisted],
      instanceBindings: [instanceBinding],
      workspaceBindings: [],
    }
    mutations.bindingUpdate.mutateAsync.mockImplementationOnce(async ({ body }) => {
      persisted = {
        ...persisted,
        accountId: body.accountId,
        modelCatalogId: body.modelCatalogId,
        extraParams: body.extraParametersJson,
      }
      return persisted
    })
    const view = render(<BindingsSection {...props} />)
    const dialog = await openAnswerDialog(user)
    await user.selectOptions(within(dialog).getByLabelText('Reasoning mode'), 'none')
    await user.click(within(dialog).getByRole('button', { name: 'Save' }))
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument())
    view.rerender(<BindingsSection {...props} bindingsForScope={[persisted]} />)
    const reopened = await openAnswerDialog(user)
    expect(within(reopened).getByLabelText('Reasoning mode')).toHaveValue('none')
    expect(
      within(reopened).getByLabelText<HTMLTextAreaElement>(/Additional parameters/).value,
    ).toContain('"keep": 7')
    expect(within(reopened).getByRole('button', { name: 'Save' })).toBeEnabled()
    expect(persisted.extraParams).toEqual({
      vendor_extension: { keep: 7 },
      reasoningMode: { version: 1, mode: 'none' },
    })
  })

  it('explains an unconfirmed pair read-only instead of offering a mode', async () => {
    const user = userEvent.setup()
    renderSection()
    const dialog = await openAnswerDialog(user)

    expect(within(dialog).getByText(/No reasoning mode is confirmed/)).toBeVisible()
    expect(within(dialog).queryByLabelText('Reasoning mode')).not.toBeInTheDocument()
  })

  it('revalidates a pending selection when the model changes and blocks until Unset', async () => {
    const user = userEvent.setup()
    renderSection({ models: [confirmedModel, secondModel], availableAccounts: [chatAccount] })
    const dialog = await openAnswerDialog(user)

    await user.selectOptions(within(dialog).getByLabelText('Reasoning mode'), 'none')
    await user.click(within(dialog).getByRole('button', { name: /Model Two/ }))

    expect(await within(dialog).findByText(/The selected mode is not confirmed/)).toBeVisible()
    const save = within(dialog).getByRole('button', { name: 'Save' })
    expect(save).toBeDisabled()

    const unset = within(dialog).getByRole('button', { name: 'Unset' })
    unset.focus()
    await user.keyboard('{Enter}')
    expect(save).toBeEnabled()
  })

  it.each(['reasoning_effort_none', 'thinking_disabled'] as const)(
    'requires a new decision when another confirmed model uses %s',
    async (wire) => {
      const user = userEvent.setup()
      const other = {
        ...secondModel,
        reasoningCapability: { ...confirmedModel.reasoningCapability, wire },
      } as AdminModelOption
      renderSection({ models: [confirmedModel, other], availableAccounts: [chatAccount] })
      const dialog = await openAnswerDialog(user)
      await user.selectOptions(within(dialog).getByLabelText('Reasoning mode'), 'none')
      await user.click(within(dialog).getByRole('button', { name: /Model Two/ }))
      const save = within(dialog).getByRole('button', { name: 'Save' })
      expect(save).toBeDisabled()
      expect(mutations.bindingCreate.mutateAsync).not.toHaveBeenCalled()
      await user.click(within(dialog).getByRole('button', { name: /Model One/ }))
      expect(save).toBeDisabled()
      await user.click(within(dialog).getByRole('button', { name: /Model Two/ }))
      await user.click(within(dialog).getByRole('button', { name: 'Confirm for this pair' }))
      expect(save).toBeEnabled()
      await user.click(save)
      await waitFor(() =>
        expect(mutations.bindingCreate.mutateAsync).toHaveBeenCalledWith(
          expect.objectContaining({
            modelCatalogId: other.id,
            extraParametersJson: { reasoningMode: { version: 1, mode: 'none' } },
          }),
        ),
      )
    },
  )

  it.each(['account', 'provider'] as const)(
    'invalidates the reasoning decision across a confirmed %s change',
    async (change) => {
      const user = userEvent.setup()
      const otherProvider =
        change === 'provider' ? { ...chatProvider, id: 'provider-2' } : chatProvider
      const otherAccount = {
        ...chatAccount,
        id: 'account-2',
        providerId: otherProvider.id,
        provider: otherProvider,
        label: 'Other account',
      }
      const firstModel = {
        ...confirmedModel,
        availableAccountIds: [chatAccount.id, otherAccount.id],
      }
      const otherModel =
        change === 'provider'
          ? {
              ...firstModel,
              id: 'model-2',
              modelName: 'Model Two',
              providerCatalogId: otherProvider.id,
            }
          : firstModel
      renderSection({
        availableAccounts: [chatAccount, otherAccount],
        models: change === 'provider' ? [firstModel, otherModel] : [firstModel],
      })
      const dialog = await openAnswerDialog(user)
      await user.selectOptions(within(dialog).getByLabelText('Reasoning mode'), 'none')
      await user.selectOptions(within(dialog).getByLabelText('Account'), otherAccount.id)
      await user.click(
        within(dialog).getByRole('button', { name: new RegExp(otherModel.modelName) }),
      )
      expect(within(dialog).getByRole('button', { name: 'Save' })).toBeDisabled()
      await user.click(within(dialog).getByRole('button', { name: 'Unset' }))
      await user.click(within(dialog).getByRole('button', { name: 'Save' }))
      await waitFor(() =>
        expect(mutations.bindingCreate.mutateAsync).toHaveBeenCalledWith(
          expect.objectContaining({ accountId: otherAccount.id, modelCatalogId: otherModel.id }),
        ),
      )
      expect(mutations.bindingCreate.mutateAsync.mock.calls[0]?.[0]).not.toHaveProperty(
        'extraParametersJson',
      )
    },
  )

  it.each([
    {
      model: confirmedModel,
      selection: { version: 2, mode: 'none' },
      error: /not a readable form/,
    },
    {
      model: secondModel,
      selection: { version: 1, mode: 'none' },
      error: /The selected mode is not confirmed/,
    },
  ])(
    'reveals a stored blocking error without manually opening parameters: $selection.version',
    async (fixture) => {
      const user = userEvent.setup()
      renderSection({
        models: [fixture.model],
        availableAccounts: [chatAccount],
        bindingsForScope: [
          {
            ...libraryBinding({ reasoningMode: fixture.selection }),
            modelCatalogId: fixture.model.id,
          },
        ],
      })
      await user.click(
        within(screen.getByRole('listitem', { name: 'Answer Generation' })).getByRole('button', {
          name: 'Edit',
        }),
      )
      const dialog = await screen.findByRole('dialog')
      expect((dialog.querySelector('details') as HTMLDetailsElement).open).toBe(true)
      expect(within(dialog).getByText(fixture.error)).toBeVisible()
      expect(within(dialog).getByRole('button', { name: 'Unset' })).toBeVisible()
    },
  )

  it('shows a saved unreadable selection honestly and only Unset resolves it', async () => {
    const user = userEvent.setup()
    renderSection({
      models: [confirmedModel],
      availableAccounts: [chatAccount],
      bindingsForScope: [libraryBinding({ reasoningMode: { version: 2, mode: 'none' } })],
    })
    const dialog = await openAnswerDialog(user)

    expect(within(dialog).getByText(/not a readable form/)).toBeVisible()
    expect(within(dialog).getByLabelText('Reasoning mode')).toHaveValue('')
    expect(within(dialog).getByLabelText('Reasoning mode')).toBeDisabled()
    const save = within(dialog).getByRole('button', { name: 'Save' })
    expect(save).toBeDisabled()

    await user.click(within(dialog).getByRole('button', { name: 'Unset' }))
    expect(save).toBeEnabled()
  })

  it('explains a raw conflict and removes only the conflicting parameter on demand', async () => {
    const user = userEvent.setup()
    renderSection({
      models: [confirmedModel],
      availableAccounts: [chatAccount],
      bindingsForScope: [
        libraryBinding({
          seed: 1,
          reasoningMode: { version: 1, mode: 'none' },
          reasoning_effort: 'high',
        }),
      ],
    })
    const dialog = await openAnswerDialog(user)

    expect(
      within(dialog).getByText(/disagrees with the confirmed reasoning_effort = none/),
    ).toBeVisible()
    const save = within(dialog).getByRole('button', { name: 'Save' })
    expect(save).toBeDisabled()

    await user.click(within(dialog).getByRole('button', { name: 'Remove `reasoning_effort`' }))
    expect(save).toBeEnabled()
    await user.click(save)
    await waitFor(() =>
      expect(mutations.bindingUpdate.mutateAsync).toHaveBeenCalledWith({
        id: 'local-1',
        body: expect.objectContaining({
          extraParametersJson: { seed: 1, reasoningMode: { version: 1, mode: 'none' } },
        }),
      }),
    )
  })

  it('rejects a reasoningMode typed into the raw JSON instead of silently honoring it', async () => {
    const user = userEvent.setup()
    renderSection({
      models: [confirmedModel],
      availableAccounts: [chatAccount],
      bindingsForScope: [libraryBinding({ seed: 1 })],
    })
    const dialog = await openAnswerDialog(user)

    const raw = within(dialog).getByLabelText(/Additional parameters/)
    fireEvent.change(raw, {
      target: { value: '{"reasoningMode": {"version": 1, "mode": "none"}}' },
    })

    expect(within(dialog).getByText(/must not set `reasoningMode`/)).toBeVisible()
    const save = within(dialog).getByRole('button', { name: 'Save' })
    expect(save).toBeDisabled()

    await user.click(within(dialog).getByRole('button', { name: 'Remove it from the JSON' }))
    expect(save).toBeEnabled()
  })

  it('preserves an invalid saved selection when removing a separate raw-editor key', async () => {
    const user = userEvent.setup()
    renderSection({
      models: [confirmedModel],
      availableAccounts: [chatAccount],
      bindingsForScope: [libraryBinding({ seed: 1, reasoningMode: { version: 2, mode: 'none' } })],
    })
    const dialog = await openAnswerDialog(user)
    fireEvent.change(within(dialog).getByLabelText(/Additional parameters/), {
      target: { value: '{"seed":1,"reasoningMode":{"version":1,"mode":"none"}}' },
    })
    await user.click(within(dialog).getByRole('button', { name: 'Remove it from the JSON' }))
    expect(within(dialog).getByText(/not a readable form/)).toBeVisible()
    expect(within(dialog).getByRole('button', { name: 'Save' })).toBeDisabled()
    await user.click(within(dialog).getByRole('button', { name: 'Unset' }))
    await user.click(within(dialog).getByRole('button', { name: 'Save' }))
    await waitFor(() =>
      expect(mutations.bindingUpdate.mutateAsync).toHaveBeenCalledWith({
        id: 'local-1',
        body: expect.objectContaining({ extraParametersJson: { seed: 1 } }),
      }),
    )
  })

  it('labels and describes the reasoning selector for assistive technology', async () => {
    const user = userEvent.setup()
    renderSection({ models: [confirmedModel], availableAccounts: [chatAccount] })
    const dialog = await openAnswerDialog(user)

    const select = within(dialog).getByLabelText('Reasoning mode')
    expect(select).toHaveAccessibleName('Reasoning mode')
    expect(select).toHaveAccessibleDescription(/Unset keeps the provider/)
    select.focus()
    expect(select).toHaveFocus()
    await user.selectOptions(select, 'none')
    expect(select).toHaveValue('none')
  })

  it('does not surface the raw backend reasoning error to the operator', async () => {
    const user = userEvent.setup()
    renderSection({
      models: [confirmedModel],
      availableAccounts: [chatAccount],
      bindingsForScope: [libraryBinding({ reasoningMode: { version: 1, mode: 'none' } })],
    })
    mutations.bindingUpdate.mutateAsync.mockRejectedValueOnce(
      new ApiError(400, {
        detail:
          'extraParametersJson.reasoningMode mode `none` is not confirmed for provider `openai` model `x`',
      }),
    )
    const dialog = await openAnswerDialog(user)

    await user.click(within(dialog).getByRole('button', { name: 'Save' }))
    expect(
      await within(dialog).findByText(/The server rejected this reasoning selection/),
    ).toBeVisible()
    expect(within(dialog).queryByText(/not confirmed for provider/)).not.toBeInTheDocument()
  })
})
