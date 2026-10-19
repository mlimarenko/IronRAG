import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { createMemoryRouter, RouterProvider } from 'react-router-dom'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import i18n from '@/shared/i18n'
import type { AIAccount, AIProvider } from '@/shared/types'
import AiAdmin from './AiAdmin'
import { AccountsSection } from './ai/AccountsSection'

const provider: AIProvider = {
  id: 'provider-alpha',
  displayName: 'Provider Alpha',
  purposeGaps: [],
  kind: 'alpha',
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
  modelDiscovery: { mode: 'credential', paths: [] },
  capabilities: {},
  runtime: {},
  uiHints: {},
  modelCount: 0,
  credentialCount: 3,
}
const configurableProvider: AIProvider = {
  ...provider,
  id: 'provider-configurable',
  displayName: 'Configurable Provider',
  kind: 'configurable',
  defaultBaseUrl: 'https://tenant.example/v1',
  apiKeyRequired: false,
  baseUrlRequired: true,
  credentialPolicy: {
    ...provider.credentialPolicy,
    apiKeyRequired: false,
    baseUrlRequired: true,
    baseUrlMode: 'required',
    validationMode: 'none',
  },
  baseUrlPolicy: {
    ...provider.baseUrlPolicy,
    allowOverride: true,
    requireHttps: false,
    allowPrivateNetwork: true,
  },
  uiHints: { documentation: 'synthetic' },
}

function account(
  id: string,
  label: string,
  scope: Pick<AIAccount, 'scopeKind' | 'workspaceId' | 'libraryId'>,
): AIAccount {
  return {
    id,
    ...scope,
    providerId: provider.id,
    providerName: provider.displayName,
    providerKind: provider.kind,
    provider,
    label,
    state: 'active',
    createdAt: '2026-01-01T00:00:00Z',
    updatedAt: '2026-01-02T00:00:00Z',
    apiKeySummary: 'configured',
  }
}

const instanceAccount = account('account-instance', 'Instance account', {
  scopeKind: 'instance',
})
const workspaceAccount = account('account-workspace', 'Workspace account', {
  scopeKind: 'workspace',
  workspaceId: 'workspace-1',
})
const libraryAccount = account('account-library', 'Library account', {
  scopeKind: 'library',
  workspaceId: 'workspace-1',
  libraryId: 'library-1',
})

/**
 * What the provider's live listing returns. The mock factory is hoisted, so
 * this has to be declared with `var` to be in scope by the time it runs.
 */
// eslint-disable-next-line no-var
var availableProviderModels = [
  {
    modelName: 'vendor/priced',
    inputModalities: ['text'],
    contextWindow: 262_144,
    maxOutputTokens: 8_192,
    pricing: {
      currencyCode: 'USD',
      inputPricePer1m: '0.4000000',
      cachedInputPricePer1m: '0.1000000',
      cacheWriteInputPricePer1m: '0.4000000',
      outputPricePer1m: '1.6000000',
    },
    modelCatalogId: null,
  },
]

const adminData = {
  providers: [provider],
  models: [],
  localAccounts: [libraryAccount],
  availableAccounts: [libraryAccount, workspaceAccount, instanceAccount],
  instanceBindings: [],
  workspaceBindings: [],
  libraryBindings: [],
  bindingsForScope: [],
  localParams: {},
  isLoading: false,
  error: null as unknown,
  refetch: vi.fn(),
}
const workspaceAdminData = {
  ...adminData,
  localAccounts: [workspaceAccount],
  availableAccounts: [workspaceAccount, instanceAccount],
}

const createMutation = () => ({ isPending: false, mutateAsync: vi.fn() })
const mutations = {
  providerCreate: createMutation(),
  providerUpdate: createMutation(),
  providerDelete: createMutation(),
  modelCreate: createMutation(),
  modelUpdate: createMutation(),
  modelDelete: createMutation(),
  accountCreate: createMutation(),
  accountUpdate: createMutation(),
  accountDelete: createMutation(),
  bindingCreate: createMutation(),
  bindingUpdate: createMutation(),
  bindingDelete: createMutation(),
  bindingValidate: createMutation(),
}
const accountDetail: {
  data: AIAccount | undefined
  isPending: boolean
  isFetching: boolean
  isError: boolean
} = {
  data: libraryAccount,
  isPending: false,
  isFetching: false,
  isError: false,
}

// The shell carries a bounded sample of libraries; an empty one is the ordinary
// case for any library past that sample, which is what a link most often names.
const { selectLibraryMock, appScope } = vi.hoisted(() => ({
  selectLibraryMock: vi.fn(),
  appScope: {
    activeLibrary: { id: 'library-1', workspaceId: 'workspace-1', name: 'Library One' },
  },
}))

vi.mock('@/shared/contexts/app-context', () => ({
  useApp: () => ({
    activeWorkspace: { id: 'workspace-1', name: 'Workspace One' },
    activeLibrary: appScope.activeLibrary,
    libraries: [],
    selectLibrary: selectLibraryMock,
  }),
}))

vi.mock('./ai/api', () => ({
  useAiAdminData: (selectedScope: string) =>
    selectedScope === 'workspace' ? workspaceAdminData : adminData,
  useAiAdminMutations: () => mutations,
  useAccountDetail: () => accountDetail,
  useProviderDetail: () => ({ data: undefined, isPending: false }),
  useModelDetail: () => ({ data: undefined, isPending: false }),
  useBindingDetail: () => ({ data: undefined, isPending: false }),
  useAvailableProviderModels: () => ({
    data: availableProviderModels,
    isFetching: false,
    isError: false,
  }),
  useBindingValidations: () => ({ data: [], isPending: false }),
  useBindingValidation: () => ({ data: undefined, isPending: false }),
}))

function renderPage(
  section: 'accounts' | 'bindings' | 'catalog',
  scope: 'workspace' | 'library' = 'library',
) {
  const router = createMemoryRouter([{ path: '/admin/ai', element: <AiAdmin /> }], {
    initialEntries: [`/admin/ai?scope=${scope}&section=${section}`],
  })
  render(<RouterProvider router={router} />)
  return router
}

async function expectAccountEditorToStayBlockedWhileLoading() {
  const user = userEvent.setup()
  render(
    <AccountsSection
      selectedScope="library"
      scopeContext={{ workspaceId: 'workspace-1', libraryId: 'library-1' }}
      accounts={[libraryAccount]}
      providers={[provider]}
    />,
  )

  await user.click(screen.getByRole('button', { name: `Edit: ${libraryAccount.label}` }))
  const dialog = screen.getByRole('dialog', { name: 'Edit account' })

  expect(within(dialog).getByText('Loading...')).toBeInTheDocument()
  expect(within(dialog).queryByLabelText('Label')).not.toBeInTheDocument()
  expect(within(dialog).getByRole('button', { name: 'Save' })).toBeDisabled()
  fireEvent.submit(dialog.querySelector('form') as HTMLFormElement)
  expect(mutations.accountUpdate.mutateAsync).not.toHaveBeenCalled()
}

describe('AiAdmin scoped accounts', () => {
  beforeEach(async () => {
    vi.clearAllMocks()
    adminData.localAccounts = [libraryAccount]
    adminData.availableAccounts = [libraryAccount, workspaceAccount, instanceAccount]
    workspaceAdminData.localAccounts = [workspaceAccount]
    workspaceAdminData.availableAccounts = [workspaceAccount, instanceAccount]
    adminData.isLoading = false
    adminData.error = null
    accountDetail.data = libraryAccount
    accountDetail.isPending = false
    accountDetail.isFetching = false
    accountDetail.isError = false
    appScope.activeLibrary = { id: 'library-1', workspaceId: 'workspace-1', name: 'Library One' }
    await i18n.changeLanguage('en')
  })

  it('opens the library a link names even when the shell never loaded it', async () => {
    // Looking the id up in the loaded sample first, and doing nothing when it
    // was absent, meant every library past the shell's bounded sample opened
    // this screen on whichever library happened to be active — reading, and
    // offering to write, another library's AI bindings under the requested
    // one's name. Absent from the sample is not absent from the catalog.
    const router = createMemoryRouter([{ path: '/admin/ai', element: <AiAdmin /> }], {
      initialEntries: ['/admin/ai?scope=library&lib=library-outside-sample'],
    })
    render(<RouterProvider router={router} />)

    await waitFor(() => {
      expect(selectLibraryMock).toHaveBeenCalledWith('library-outside-sample')
    })
    // Resolution has only STARTED — the shell is still on the previous library.
    // Erasing the link here would drop the instruction whenever the lookup
    // fails, stranding this screen on the wrong library with nothing said.
    expect(new URLSearchParams(router.state.location.search).get('lib')).toBe(
      'library-outside-sample',
    )
  })

  it('stops re-applying the link once the shell has landed on it', async () => {
    // A link says "go here once"; it is not a standing binding. Left in the
    // address it re-applied on every later scope change, so the scope switcher
    // on this screen was inert: each workspace the person picked was pulled
    // straight back to the linked library.
    appScope.activeLibrary = {
      id: 'library-outside-sample',
      workspaceId: 'workspace-2',
      name: 'Deep Library',
    }
    const router = createMemoryRouter([{ path: '/admin/ai', element: <AiAdmin /> }], {
      initialEntries: ['/admin/ai?scope=library&lib=library-outside-sample'],
    })
    render(<RouterProvider router={router} />)

    await waitFor(() => {
      expect(new URLSearchParams(router.state.location.search).get('lib')).toBeNull()
    })
    expect(selectLibraryMock).not.toHaveBeenCalled()
    // Only the link is consumed; the rest of the address is left alone.
    expect(new URLSearchParams(router.state.location.search).get('scope')).toBe('library')
  })

  it('shows local and inherited visible accounts but only offers mutations for the local account', async () => {
    const user = userEvent.setup()
    renderPage('accounts')

    expect(screen.getByText(libraryAccount.label)).toBeInTheDocument()
    expect(screen.getByText(workspaceAccount.label)).toBeInTheDocument()
    expect(screen.getByText(instanceAccount.label)).toBeInTheDocument()

    expect(
      screen.getByRole('button', { name: `Edit: ${libraryAccount.label}` }),
    ).toBeInTheDocument()
    expect(
      screen.queryByRole('button', { name: `Delete: ${libraryAccount.label}` }),
    ).not.toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: libraryAccount.label }))
    expect(screen.getByRole('button', { name: `Delete: ${libraryAccount.label}` })).toBeVisible()
    expect(
      screen.queryByRole('button', { name: `Edit: ${workspaceAccount.label}` }),
    ).not.toBeInTheDocument()
    expect(
      screen.queryByRole('button', { name: `Delete: ${instanceAccount.label}` }),
    ).not.toBeInTheDocument()
    expect(
      within(screen.getByRole('list', { name: 'Accounts' })).getAllByText(/Read-only/i),
    ).toHaveLength(2)
  })

  it('keeps inherited instance accounts read-only at workspace scope', () => {
    renderPage('accounts', 'workspace')

    expect(screen.getByText(workspaceAccount.label)).toBeInTheDocument()
    expect(screen.getByText(instanceAccount.label)).toBeInTheDocument()
    expect(screen.queryByText(libraryAccount.label)).not.toBeInTheDocument()
    expect(
      screen.getByRole('button', { name: `Edit: ${workspaceAccount.label}` }),
    ).toBeInTheDocument()
    expect(
      screen.queryByRole('button', { name: `Edit: ${instanceAccount.label}` }),
    ).not.toBeInTheDocument()
    expect(
      within(screen.getByRole('listitem', { name: instanceAccount.label })).getByText(/Read-only/i),
    ).toBeInTheDocument()
  })

  it('closes a local account editor when its scope changes', async () => {
    const user = userEvent.setup()
    const router = renderPage('accounts')

    await user.click(screen.getByRole('button', { name: `Edit: ${libraryAccount.label}` }))
    expect(screen.getByRole('dialog', { name: 'Edit account' })).toBeInTheDocument()

    await act(async () => {
      await router.navigate('/admin/ai?scope=workspace&section=accounts')
    })

    expect(screen.queryByRole('dialog', { name: 'Edit account' })).not.toBeInTheDocument()
    expect(mutations.accountUpdate.mutateAsync).not.toHaveBeenCalled()
  })

  it('rechecks account locality at save time', async () => {
    const user = userEvent.setup()
    const view = render(
      <AccountsSection
        selectedScope="library"
        scopeContext={{ workspaceId: 'workspace-1', libraryId: 'library-1' }}
        accounts={[libraryAccount]}
        providers={[provider]}
      />,
    )

    await user.click(screen.getByRole('button', { name: `Edit: ${libraryAccount.label}` }))
    view.rerender(
      <AccountsSection
        selectedScope="workspace"
        scopeContext={{ workspaceId: 'workspace-1' }}
        accounts={[libraryAccount]}
        providers={[provider]}
      />,
    )
    await user.click(screen.getByRole('button', { name: 'Save' }))

    expect(mutations.accountUpdate.mutateAsync).not.toHaveBeenCalled()
    expect(
      screen.getByText('This account can only be changed at its own scope. Reopen it there.'),
    ).toBeInTheDocument()
  })

  it('rejects a stale local list row when authoritative detail moved scope', async () => {
    const user = userEvent.setup()
    accountDetail.data = {
      ...libraryAccount,
      scopeKind: 'workspace',
    }
    delete accountDetail.data.libraryId
    render(
      <AccountsSection
        selectedScope="library"
        scopeContext={{ workspaceId: 'workspace-1', libraryId: 'library-1' }}
        accounts={[libraryAccount]}
        providers={[provider]}
      />,
    )

    await user.click(screen.getByRole('button', { name: `Edit: ${libraryAccount.label}` }))
    await user.click(screen.getByRole('button', { name: 'Save' }))

    expect(mutations.accountUpdate.mutateAsync).not.toHaveBeenCalled()
    expect(
      screen.getByText('This account can only be changed at its own scope. Reopen it there.'),
    ).toBeInTheDocument()
  })

  it('consumes the recommended add-account request after the dialog is closed', async () => {
    const user = userEvent.setup()
    adminData.localAccounts = []
    adminData.availableAccounts = []
    const router = renderPage('bindings')

    await user.click(screen.getByRole('button', { name: 'Add account' }))
    expect(screen.getByRole('dialog', { name: 'Add account' })).toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'Cancel' }))

    await act(async () => {
      await router.navigate('/admin/ai?scope=workspace&section=accounts')
    })

    expect(screen.queryByRole('dialog', { name: 'Add account' })).not.toBeInTheDocument()
  })

  it('shows what a discovered model costs and holds, not a row of dashes', async () => {
    // The picker read these columns from its own props and the props were built
    // from the API response — and the mapping in between carried only the name
    // and the modalities. Every cell in a four-hundred-row table was a dash
    // while the API was returning the numbers. Component tests could not see
    // it, because they build the option objects directly.
    const user = userEvent.setup()
    renderPage('bindings')

    const binding = screen.getByRole('listitem', { name: 'Document Understanding' })
    await user.click(within(binding).getByRole('button', { name: 'Set up' }))
    const dialog = await screen.findByRole('dialog')
    await user.selectOptions(
      within(dialog).getAllByRole('combobox')[0] as HTMLElement,
      instanceAccount.id,
    )

    const row = await screen.findByRole('row', { name: /vendor\/priced/ })
    expect(within(row).getByText('262K')).toBeInTheDocument()
    expect(within(row).getByText('8K')).toBeInTheDocument()
    expect(within(row).getByText('0.4')).toBeInTheDocument()
    expect(within(row).getByText('1.6')).toBeInTheDocument()
    expect(within(row).getByText('USD')).toBeInTheDocument()
  })

  it.each([
    ['library', 3],
    ['workspace', 2],
  ] as const)('counts all visible accounts in provider usage at %s scope', async (scope, count) => {
    const user = userEvent.setup()
    renderPage('catalog', scope)
    await user.click(screen.getByRole('tab', { name: 'Providers' }))

    expect(screen.getByText(`0 models · ${count} accounts`)).toBeInTheDocument()
  })

  it('keeps catalog selection in the URL and restores it on history navigation', async () => {
    const user = userEvent.setup()
    const router = renderPage('catalog')
    expect(screen.getByRole('tab', { name: 'Models' })).toHaveAttribute('aria-selected', 'true')
    await user.click(screen.getByRole('tab', { name: 'Providers' }))
    expect(router.state.location.search).toContain('catalog=providers')
    await user.click(screen.getByRole('tab', { name: 'Models' }))
    expect(router.state.location.search).not.toContain('catalog=')
    await act(async () => {
      await router.navigate(-1)
    })
    expect(screen.getByRole('tab', { name: 'Providers' })).toHaveAttribute('aria-selected', 'true')
    await act(async () => {
      await router.navigate(1)
    })
    expect(screen.getByRole('tab', { name: 'Models' })).toHaveAttribute('aria-selected', 'true')
  })

  it('renders the empty visible-account state instead of stale inherited rows', () => {
    adminData.localAccounts = []
    adminData.availableAccounts = []

    renderPage('accounts')

    expect(screen.getByText('No accounts are visible at this scope yet.')).toBeInTheDocument()
    expect(screen.queryByText(workspaceAccount.label)).not.toBeInTheDocument()
    const accountsPanel = screen
      .getByRole('heading', { name: 'Accounts in this scope' })
      .closest('section')
    expect(
      within(accountsPanel as HTMLElement).getByRole('button', { name: 'Add account' }),
    ).toBeInTheDocument()
  })

  it('keeps scoped content hidden while AI configuration is loading', () => {
    adminData.isLoading = true

    renderPage('accounts')

    expect(document.querySelector('.aicfg-loading-state')).toBeInTheDocument()
    expect(screen.queryByText(libraryAccount.label)).not.toBeInTheDocument()
  })

  it('offers a working retry action when scoped AI configuration fails to load', async () => {
    const user = userEvent.setup()
    adminData.error = new Error('Synthetic load failure')

    renderPage('accounts')
    await user.click(screen.getByRole('button', { name: 'Try again' }))

    expect(adminData.refetch).toHaveBeenCalledTimes(1)
    expect(screen.queryByText(libraryAccount.label)).not.toBeInTheDocument()
  })

  it('changes scopes and sections through the visible controls and opens the setup wizard', async () => {
    const user = userEvent.setup()
    const router = renderPage('accounts')

    await user.click(screen.getByRole('button', { name: 'Instance' }))
    expect(router.state.location.search).toContain('scope=instance')
    await user.click(screen.getByRole('button', { name: 'Workspace One' }))
    expect(router.state.location.search).toContain('scope=workspace')
    await user.click(screen.getByRole('button', { name: 'Library One' }))
    expect(router.state.location.search).toContain('scope=library')

    // The section strip is a tablist, so a screen reader announces which
    // section is current. Querying by role here is what keeps that true.
    await user.click(screen.getByRole('tab', { name: 'Bindings' }))
    expect(router.state.location.search).toContain('section=bindings')
    expect(screen.getByRole('tab', { name: 'Bindings' })).toHaveAttribute('aria-selected', 'true')
    await user.click(screen.getByRole('tab', { name: 'Catalog' }))
    expect(router.state.location.search).toContain('section=catalog')
    expect(screen.getByRole('tab', { name: 'Bindings' })).toHaveAttribute('aria-selected', 'false')
    await user.click(screen.getByRole('tab', { name: 'Accounts' }))
    expect(router.state.location.search).toContain('section=accounts')

    await user.click(screen.getByRole('button', { name: 'Setup wizard' }))
    expect(screen.getByRole('dialog', { name: 'Set up an AI binding' })).toBeInTheDocument()
    expect(router.state.location.search).toContain('wizard=1')
    await user.click(screen.getByRole('button', { name: 'Close' }))
    await waitFor(() => {
      expect(screen.queryByRole('dialog', { name: 'Set up an AI binding' })).not.toBeInTheDocument()
    })
    expect(router.state.location.search).not.toContain('wizard=1')
  })

  it('creates a local account with the selected library scope and fixed provider endpoint', async () => {
    const user = userEvent.setup()
    render(
      <AccountsSection
        selectedScope="library"
        scopeContext={{ workspaceId: 'workspace-1', libraryId: 'library-1' }}
        accounts={[workspaceAccount, instanceAccount]}
        providers={[provider]}
      />,
    )

    await user.click(screen.getByRole('button', { name: 'Add account' }))
    const dialog = screen.getByRole('dialog', { name: 'Add account' })
    await user.selectOptions(within(dialog).getByLabelText('Provider'), provider.id)
    await user.type(within(dialog).getByLabelText('Label'), 'Synthetic library account')
    await user.type(within(dialog).getByPlaceholderText('Provider token'), 'synthetic-token')

    const baseUrl = within(dialog).getByLabelText(/^Base URL/)
    expect(baseUrl).toBeDisabled()
    expect(baseUrl).toHaveValue(provider.defaultBaseUrl)

    await user.click(within(dialog).getByRole('button', { name: 'Save' }))

    expect(mutations.accountCreate.mutateAsync).toHaveBeenCalledWith({
      scopeKind: 'library',
      workspaceId: 'workspace-1',
      libraryId: 'library-1',
      providerCatalogId: provider.id,
      label: 'Synthetic library account',
      apiKey: 'synthetic-token',
      baseUrl: undefined,
    })
    expect(screen.queryByRole('dialog', { name: 'Add account' })).not.toBeInTheDocument()
  })

  it('updates a local account without replacing an omitted secret or fixed endpoint', async () => {
    const user = userEvent.setup()
    render(
      <AccountsSection
        selectedScope="library"
        scopeContext={{ workspaceId: 'workspace-1', libraryId: 'library-1' }}
        accounts={[libraryAccount, workspaceAccount]}
        providers={[provider]}
      />,
    )

    await user.click(screen.getByRole('button', { name: `Edit: ${libraryAccount.label}` }))
    const dialog = screen.getByRole('dialog', { name: 'Edit account' })
    expect(within(dialog).getByLabelText('Provider')).toBeDisabled()
    expect(within(dialog).getByPlaceholderText('Leave blank to keep current secret')).toHaveValue(
      '',
    )
    await user.clear(within(dialog).getByLabelText('Label'))
    await user.type(within(dialog).getByLabelText('Label'), 'Renamed local account')
    await user.selectOptions(within(dialog).getByLabelText('Status'), 'invalid')
    await user.click(within(dialog).getByRole('button', { name: 'Save' }))

    expect(mutations.accountUpdate.mutateAsync).toHaveBeenCalledWith({
      id: libraryAccount.id,
      body: {
        label: 'Renamed local account',
        credentialState: 'invalid',
        apiKey: undefined,
        baseUrl: undefined,
        expectedUpdatedAt: libraryAccount.updatedAt,
      },
    })
  })

  it('creates an account with an editable required endpoint and optional credential', async () => {
    const user = userEvent.setup()
    render(
      <AccountsSection
        selectedScope="instance"
        scopeContext={{}}
        accounts={[]}
        providers={[configurableProvider]}
      />,
    )

    await user.click(screen.getByRole('button', { name: 'Add account' }))
    const dialog = screen.getByRole('dialog', { name: 'Add account' })
    await user.selectOptions(within(dialog).getByLabelText('Provider'), configurableProvider.id)
    await user.type(within(dialog).getByLabelText('Label'), 'Synthetic instance account')
    expect(within(dialog).getByText('Token (optional)')).toBeInTheDocument()
    expect(
      within(dialog).getByText('HTTP endpoint; private networks are allowed.'),
    ).toBeInTheDocument()
    expect(within(dialog).getByText('Technical details')).toBeInTheDocument()

    const baseUrl = within(dialog).getByDisplayValue(configurableProvider.defaultBaseUrl ?? '')
    expect(baseUrl).toBeEnabled()
    expect(baseUrl).toBeRequired()
    await user.clear(baseUrl)
    await user.type(baseUrl, 'https://tenant.example/v2')
    await user.click(within(dialog).getByRole('button', { name: 'Save' }))

    expect(mutations.accountCreate.mutateAsync).toHaveBeenCalledWith({
      scopeKind: 'instance',
      providerCatalogId: configurableProvider.id,
      label: 'Synthetic instance account',
      apiKey: undefined,
      baseUrl: 'https://tenant.example/v2',
    })
  })

  it('shows account-detail loading state before exposing the edit fields', async () => {
    accountDetail.isPending = true

    await expectAccountEditorToStayBlockedWhileLoading()
  })

  it('does not expose a cached account detail while its authoritative refetch is running', async () => {
    accountDetail.data = libraryAccount
    accountDetail.isFetching = true

    await expectAccountEditorToStayBlockedWhileLoading()
  })

  it('blocks account mutation when authoritative detail loading fails', async () => {
    const user = userEvent.setup()
    accountDetail.data = undefined
    accountDetail.isError = true
    render(
      <AccountsSection
        selectedScope="library"
        scopeContext={{ workspaceId: 'workspace-1', libraryId: 'library-1' }}
        accounts={[libraryAccount]}
        providers={[provider]}
      />,
    )

    await user.click(screen.getByRole('button', { name: `Edit: ${libraryAccount.label}` }))
    const dialog = screen.getByRole('dialog', { name: 'Edit account' })

    expect(within(dialog).getByRole('alert')).toHaveTextContent('This view could not be loaded')
    expect(within(dialog).queryByLabelText('Label')).not.toBeInTheDocument()
    expect(within(dialog).getByRole('button', { name: 'Save' })).toBeDisabled()
    fireEvent.submit(dialog.querySelector('form') as HTMLFormElement)
    expect(mutations.accountUpdate.mutateAsync).not.toHaveBeenCalled()
  })

  it('hydrates an edit once from matching authoritative detail without overwriting later input', async () => {
    const user = userEvent.setup()
    accountDetail.data = workspaceAccount
    const view = render(
      <AccountsSection
        selectedScope="library"
        scopeContext={{ workspaceId: 'workspace-1', libraryId: 'library-1' }}
        accounts={[libraryAccount]}
        providers={[provider]}
      />,
    )

    await user.click(screen.getByRole('button', { name: `Edit: ${libraryAccount.label}` }))
    const dialog = screen.getByRole('dialog', { name: 'Edit account' })
    expect(within(dialog).getByText('Loading...')).toBeInTheDocument()
    expect(within(dialog).getByRole('button', { name: 'Save' })).toBeDisabled()

    const authoritativeAccount: AIAccount = {
      ...libraryAccount,
      label: 'Authoritative account label',
      state: 'invalid',
      updatedAt: '2026-02-01T00:00:00Z',
    }
    accountDetail.data = authoritativeAccount
    view.rerender(
      <AccountsSection
        selectedScope="library"
        scopeContext={{ workspaceId: 'workspace-1', libraryId: 'library-1' }}
        accounts={[libraryAccount]}
        providers={[provider]}
      />,
    )

    const label = await within(dialog).findByLabelText('Label')
    expect(label).toHaveValue('Authoritative account label')
    expect(within(dialog).getByLabelText('Status')).toHaveValue('invalid')
    expect(within(dialog).getByRole('button', { name: 'Save' })).toBeEnabled()

    await user.clear(label)
    await user.type(label, 'Operator draft')
    accountDetail.data = {
      ...authoritativeAccount,
      label: 'Background refetch label',
      state: 'revoked',
      updatedAt: '2026-02-02T00:00:00Z',
    }
    view.rerender(
      <AccountsSection
        selectedScope="library"
        scopeContext={{ workspaceId: 'workspace-1', libraryId: 'library-1' }}
        accounts={[libraryAccount]}
        providers={[provider]}
      />,
    )

    expect(within(dialog).getByLabelText('Label')).toHaveValue('Operator draft')
    expect(within(dialog).getByLabelText('Status')).toHaveValue('revoked')
    await user.click(within(dialog).getByRole('button', { name: 'Save' }))
    expect(mutations.accountUpdate.mutateAsync).toHaveBeenCalledWith({
      id: libraryAccount.id,
      body: {
        label: 'Operator draft',
        credentialState: 'revoked',
        apiKey: undefined,
        baseUrl: undefined,
        expectedUpdatedAt: authoritativeAccount.updatedAt,
      },
    })
  })

  it('honors delete confirmation before mutating a local account', async () => {
    const user = userEvent.setup()
    const confirm = vi.spyOn(window, 'confirm').mockReturnValueOnce(false).mockReturnValueOnce(true)
    render(
      <AccountsSection
        selectedScope="library"
        scopeContext={{ workspaceId: 'workspace-1', libraryId: 'library-1' }}
        accounts={[libraryAccount]}
        providers={[provider]}
      />,
    )
    await user.click(screen.getByRole('button', { name: libraryAccount.label }))
    const deleteButton = screen.getByRole('button', { name: `Delete: ${libraryAccount.label}` })

    await user.click(deleteButton)
    expect(mutations.accountDelete.mutateAsync).not.toHaveBeenCalled()
    await user.click(deleteButton)

    expect(confirm).toHaveBeenCalledTimes(2)
    expect(mutations.accountDelete.mutateAsync).toHaveBeenCalledWith(libraryAccount.id)
    confirm.mockRestore()
  })

  it('keeps the create dialog open and explains a failed account save', async () => {
    const user = userEvent.setup()
    mutations.accountCreate.mutateAsync.mockRejectedValueOnce(new Error('Synthetic save failure'))
    render(
      <AccountsSection
        selectedScope="workspace"
        scopeContext={{ workspaceId: 'workspace-1' }}
        accounts={[instanceAccount]}
        providers={[provider]}
      />,
    )

    await user.click(screen.getByRole('button', { name: 'Add account' }))
    const dialog = screen.getByRole('dialog', { name: 'Add account' })
    await user.selectOptions(within(dialog).getByLabelText('Provider'), provider.id)
    await user.type(within(dialog).getByLabelText('Label'), 'Synthetic failing account')
    await user.type(within(dialog).getByPlaceholderText('Provider token'), 'synthetic-token')
    await user.click(within(dialog).getByRole('button', { name: 'Save' }))

    await waitFor(() => {
      expect(within(dialog).getByText('Synthetic save failure')).toBeInTheDocument()
    })
    expect(mutations.accountCreate.mutateAsync).toHaveBeenCalledWith(
      expect.objectContaining({ scopeKind: 'workspace', workspaceId: 'workspace-1' }),
    )
    expect(screen.getByRole('dialog', { name: 'Add account' })).toBeInTheDocument()
  })
})
