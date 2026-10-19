import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { render, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { createMemoryRouter, RouterProvider } from 'react-router-dom'
import { toast } from 'sonner'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import i18n from '@/shared/i18n'
import type {
  CatalogLibraryResponse,
  CatalogWorkspaceResponse,
  TokenResponse,
} from '@/shared/api/generated'
import AccessAdmin from './AccessAdmin'

const catalogWorkspace: CatalogWorkspaceResponse = {
  displayName: 'Workspace One',
  id: 'workspace-1',
  lifecycleState: 'active',
  slug: 'workspace-one',
}

const mintLibrary = {
  displayName: 'Library One',
  id: 'library-1',
  slug: 'library-one',
  workspaceId: catalogWorkspace.id,
} as CatalogLibraryResponse

const activeToken: TokenResponse = {
  expiresAt: '2099-01-01T00:00:00Z',
  grants: [],
  issuer: { displayLabel: 'Ada Lovelace', principalId: 'user-self' },
  label: 'Active automation',
  lastUsedAt: '2026-08-01T00:00:00Z',
  principalId: 'token-active',
  scope: {
    kind: 'workspace',
    libraries: [],
    workspace: { displayName: catalogWorkspace.displayName, id: catalogWorkspace.id },
  },
  status: 'active',
  tokenPrefix: 'irn_act',
}

const expiredToken: TokenResponse = {
  expiresAt: '2020-01-01T00:00:00Z',
  grants: [],
  label: 'Expired reader',
  principalId: 'token-expired',
  scope: {
    kind: 'library',
    libraries: [
      {
        displayName: mintLibrary.displayName,
        id: mintLibrary.id,
        workspaceId: catalogWorkspace.id,
      },
    ],
    workspace: { displayName: catalogWorkspace.displayName, id: catalogWorkspace.id },
  },
  status: 'active',
  tokenPrefix: 'irn_exp',
}

const revokedToken: TokenResponse = {
  grants: [],
  label: 'Revoked writer',
  principalId: 'token-revoked',
  revokedAt: '2026-07-01T00:00:00Z',
  scope: {
    kind: 'workspace',
    libraries: [],
    workspace: { displayName: catalogWorkspace.displayName, id: catalogWorkspace.id },
  },
  status: 'revoked',
  tokenPrefix: 'irn_rev',
}

const mocks = vi.hoisted(() => {
  const state: {
    detailState: 'loading' | 'error' | 'ready'
    librariesTotal: number
    listState: 'loading' | 'error' | 'ready'
    tokens: TokenResponse[]
  } = {
    detailState: 'ready',
    librariesTotal: 1,
    listState: 'ready',
    tokens: [],
  }
  return {
    ...state,
    deleteToken: vi.fn(),
    mintToken: vi.fn(),
    patchToken: vi.fn(),
  }
})

vi.mock('sonner', () => ({
  toast: {
    error: vi.fn(),
    success: vi.fn(),
    warning: vi.fn(),
  },
}))

vi.mock('@/shared/contexts/app-context', () => ({
  useApp: () => ({
    activeWorkspace: { id: 'workspace-1', name: 'Workspace One' },
    locale: 'en',
  }),
}))

vi.mock('./api/access', () => ({
  useAccessMutations: () => ({
    deleteToken: { isPending: false, mutate: mocks.deleteToken },
    mintToken: { isPending: false, mutate: mocks.mintToken },
    patchToken: { isPending: false, mutate: mocks.patchToken },
  }),
}))

vi.mock('@/shared/api', () => ({
  queries: {
    getIamTokenOptions: ({ path }: { path: { tokenId: string } }) => ({
      queryFn: async () => {
        if (mocks.detailState === 'loading') return await new Promise(() => undefined)
        if (mocks.detailState === 'error') throw new Error('token detail failed')
        return mocks.tokens.find((token) => token.principalId === path.tokenId) ?? activeToken
      },
      queryKey: ['admin', 'iam-token', path.tokenId],
    }),
    listCatalogLibrariesOptions: () => ({
      queryFn: async () => ({
        items: [mintLibrary],
        limit: 200,
        offset: 0,
        totalCount: mocks.librariesTotal,
      }),
      queryKey: ['admin', 'access-libraries'],
    }),
    listCatalogWorkspacesOptions: () => ({
      queryFn: async () => [catalogWorkspace],
      queryKey: ['admin', 'access-workspaces'],
    }),
    listIamTokensOptions: () => ({
      queryFn: async () => {
        if (mocks.listState === 'loading') return await new Promise(() => undefined)
        if (mocks.listState === 'error') throw new Error('tokens failed')
        return mocks.tokens
      },
      queryKey: ['admin', 'iam-tokens'],
    }),
  },
}))

function renderAccess() {
  const queryClient = new QueryClient({
    defaultOptions: { mutations: { retry: false }, queries: { retry: false } },
  })
  const router = createMemoryRouter([{ element: <AccessAdmin />, path: '/admin/access' }], {
    initialEntries: ['/admin/access'],
  })
  return render(
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  )
}

function tokenTable(): HTMLElement {
  return screen.getByRole('table', { name: 'API tokens' })
}

async function waitForTokenList() {
  // The table's header is mounted in every phase now (a column menu stays
  // reachable while the body shows loading/empty/error), so waiting for the
  // table would be vacuous. Wait for a rendered row.
  await screen.findByText(mocks.tokens[0]?.label ?? 'No tokens found.')
}

describe('AccessAdmin', () => {
  beforeEach(async () => {
    vi.clearAllMocks()
    mocks.listState = 'ready'
    mocks.detailState = 'ready'
    mocks.librariesTotal = 1
    mocks.tokens = [activeToken, expiredToken, revokedToken]
    mocks.mintToken.mockImplementation((_variables, options) => {
      options?.onSuccess?.({ token: 'plaintext-token-value' })
    })
    mocks.patchToken.mockImplementation((_variables, options) => options?.onSuccess?.({}))
    mocks.deleteToken.mockImplementation((_variables, options) => options?.onSuccess?.({}))
    await i18n.changeLanguage('en')
  })

  it('shows the loading state', async () => {
    mocks.listState = 'loading'
    renderAccess()
    expect(await screen.findByText('Loading')).toBeInTheDocument()
  })

  it('retries after a token list failure', async () => {
    const user = userEvent.setup()
    mocks.listState = 'error'
    renderAccess()

    expect(await screen.findByText('This view could not be loaded')).toBeInTheDocument()
    expect(screen.getByText('tokens failed')).toBeInTheDocument()
    mocks.listState = 'ready'
    await user.click(screen.getByRole('button', { name: 'Try again' }))
    await waitForTokenList()
    expect(within(tokenTable()).getByText(activeToken.label)).toBeInTheDocument()
  })

  it('shows the empty token directory', async () => {
    mocks.tokens = []
    renderAccess()
    expect(await screen.findByText('No tokens found.')).toBeInTheDocument()
  })

  it('lists tokens in active, expired, and revoked statuses', async () => {
    renderAccess()

    await waitForTokenList()
    expect(within(tokenTable()).getByText(activeToken.label)).toBeInTheDocument()
    expect(within(tokenTable()).getByText(expiredToken.label)).toBeInTheDocument()
    expect(within(tokenTable()).getByText(revokedToken.label)).toBeInTheDocument()
    expect(screen.getAllByText('Active').length).toBeGreaterThan(0)
    expect(screen.getAllByText('Expired').length).toBeGreaterThan(0)
    expect(screen.getAllByText('revoked').length).toBeGreaterThan(0)
  })

  it('filters by selectable status values in the common column menu', async () => {
    const user = userEvent.setup()
    renderAccess()
    await waitForTokenList()

    await user.click(
      within(screen.getByRole('columnheader', { name: /Status/ })).getByRole('button'),
    )
    await user.click(screen.getByRole('menuitemcheckbox', { name: /Expired/ }))
    await user.keyboard('{Escape}')
    expect(screen.getAllByText(expiredToken.label).length).toBeGreaterThan(0)
    expect(screen.queryByText(activeToken.label)).not.toBeInTheDocument()
    expect(screen.queryByText(revokedToken.label)).not.toBeInTheDocument()
  })

  it('filters tokens by search', async () => {
    const user = userEvent.setup()
    renderAccess()
    await waitForTokenList()

    await user.type(screen.getByRole('searchbox'), 'revoked')
    expect(screen.getAllByText(revokedToken.label).length).toBeGreaterThan(0)
    expect(screen.queryByText(activeToken.label)).not.toBeInTheDocument()
  })

  it('shows token details loading', async () => {
    const user = userEvent.setup()
    mocks.detailState = 'loading'
    renderAccess()
    await waitForTokenList()
    await user.click(within(tokenTable()).getAllByRole('button', { name: 'Details' })[0]!)

    const drawer = await screen.findByRole('dialog', { name: activeToken.label })
    expect(within(drawer).getByText('Loading')).toBeInTheDocument()
  })

  it('shows a token details error', async () => {
    const user = userEvent.setup()
    mocks.detailState = 'error'
    renderAccess()
    await waitForTokenList()
    await user.click(within(tokenTable()).getAllByRole('button', { name: 'Details' })[0]!)

    expect(await screen.findByText('Failed to load token details.')).toBeInTheDocument()
  })

  it('shows ready token details', async () => {
    const user = userEvent.setup()
    renderAccess()
    await waitForTokenList()
    await user.click(within(tokenTable()).getAllByRole('button', { name: 'Details' })[0]!)

    const drawer = await screen.findByRole('dialog', { name: activeToken.label })
    expect(await within(drawer).findByText(activeToken.tokenPrefix)).toBeInTheDocument()
    expect(within(drawer).getByText(activeToken.principalId)).toBeInTheDocument()
    expect(within(drawer).getByDisplayValue(activeToken.label)).toBeInTheDocument()
  })

  it('does not patch a token with an empty label', async () => {
    const user = userEvent.setup()
    renderAccess()
    await waitForTokenList()
    await user.click(within(tokenTable()).getAllByRole('button', { name: 'Details' })[0]!)
    const drawer = await screen.findByRole('dialog', { name: activeToken.label })
    const label = await within(drawer).findByDisplayValue(activeToken.label)
    await user.clear(label)

    expect(within(drawer).getByRole('button', { name: 'Save' })).toBeDisabled()
    expect(mocks.patchToken).not.toHaveBeenCalled()
  })

  it('patches a token label', async () => {
    const user = userEvent.setup()
    renderAccess()
    await waitForTokenList()
    await user.click(within(tokenTable()).getAllByRole('button', { name: 'Details' })[0]!)
    const drawer = await screen.findByRole('dialog', { name: activeToken.label })
    const label = await within(drawer).findByDisplayValue(activeToken.label)
    await user.clear(label)
    await user.type(label, 'Renamed token')
    await user.click(within(drawer).getByRole('button', { name: 'Save' }))

    expect(mocks.patchToken).toHaveBeenCalledWith(
      {
        body: { expiresAt: '2099-01-01T00:00:00.000Z', label: 'Renamed token' },
        path: { tokenId: activeToken.principalId },
      },
      expect.objectContaining({ onSuccess: expect.any(Function), onError: expect.any(Function) }),
    )
    expect(toast.success).toHaveBeenCalledWith('Token updated.')
  })

  it('explains a failed token patch', async () => {
    const user = userEvent.setup()
    mocks.patchToken.mockImplementation((_variables, options) => {
      options?.onError?.(new Error('patch failed'))
    })
    renderAccess()
    await waitForTokenList()
    await user.click(within(tokenTable()).getAllByRole('button', { name: 'Details' })[0]!)
    const drawer = await screen.findByRole('dialog', { name: activeToken.label })
    await user.click(await within(drawer).findByRole('button', { name: 'Save' }))

    expect(toast.error).toHaveBeenCalledWith('patch failed')
  })

  it('cancels token revocation', async () => {
    const user = userEvent.setup()
    renderAccess()
    await waitForTokenList()
    await user.click(within(tokenTable()).getAllByRole('button', { name: 'Details' })[0]!)
    const drawer = await screen.findByRole('dialog', { name: activeToken.label })
    await user.click(await within(drawer).findByRole('button', { name: 'Revoke & delete' }))
    const confirm = await screen.findByRole('dialog', { name: 'Revoke and delete token' })
    await user.click(within(confirm).getByRole('button', { name: 'Cancel' }))

    await waitFor(() => {
      expect(
        screen.queryByRole('dialog', { name: 'Revoke and delete token' }),
      ).not.toBeInTheDocument()
    })
    expect(mocks.deleteToken).not.toHaveBeenCalled()
  })

  it('revokes a token', async () => {
    const user = userEvent.setup()
    renderAccess()
    await waitForTokenList()
    await user.click(within(tokenTable()).getAllByRole('button', { name: 'Details' })[0]!)
    const drawer = await screen.findByRole('dialog', { name: activeToken.label })
    await user.click(await within(drawer).findByRole('button', { name: 'Revoke & delete' }))
    const confirm = await screen.findByRole('dialog', { name: 'Revoke and delete token' })
    await user.click(within(confirm).getByRole('button', { name: 'Revoke & delete' }))

    expect(mocks.deleteToken).toHaveBeenCalledWith(
      { path: { tokenId: activeToken.principalId } },
      expect.objectContaining({ onSuccess: expect.any(Function), onError: expect.any(Function) }),
    )
    expect(toast.success).toHaveBeenCalledWith('Token revoked and deleted.')
  })

  it('explains a failed token revoke', async () => {
    const user = userEvent.setup()
    mocks.deleteToken.mockImplementation((_variables, options) => {
      options?.onError?.(new Error('revoke failed'))
    })
    renderAccess()
    await waitForTokenList()
    await user.click(within(tokenTable()).getAllByRole('button', { name: 'Details' })[0]!)
    const drawer = await screen.findByRole('dialog', { name: activeToken.label })
    await user.click(await within(drawer).findByRole('button', { name: 'Revoke & delete' }))
    const confirm = await screen.findByRole('dialog', { name: 'Revoke and delete token' })
    await user.click(within(confirm).getByRole('button', { name: 'Revoke & delete' }))

    expect(toast.error).toHaveBeenCalledWith('revoke failed')
  })

  it('blocks minting until the form is complete', async () => {
    const user = userEvent.setup()
    renderAccess()
    await user.click(await screen.findByRole('button', { name: 'Create token' }))
    const dialog = await screen.findByRole('dialog', { name: 'Create token' })

    expect(
      within(dialog).getByText(/Enter a label, select a workspace and at least one permission/),
    ).toBeInTheDocument()
    expect(within(dialog).getByRole('button', { name: 'Create token' })).toBeDisabled()
  })

  it('mints a workspace token, reveals it, copies it, and closes', async () => {
    const user = userEvent.setup()
    const writeText = vi.fn().mockResolvedValue(undefined)
    Object.defineProperty(navigator, 'clipboard', {
      configurable: true,
      value: { writeText },
    })
    renderAccess()
    await user.click(await screen.findByRole('button', { name: 'Create token' }))
    const dialog = await screen.findByRole('dialog', { name: 'Create token' })
    await user.type(
      within(dialog).getByText('Label').parentElement!.querySelectorAll('input')[0]!,
      'CI token',
    )
    await user.click(within(dialog).getByText('Workspace read'))
    await user.click(within(dialog).getByRole('button', { name: 'Create token' }))

    expect(mocks.mintToken).toHaveBeenCalledWith(
      {
        body: expect.objectContaining({
          label: 'CI token',
          libraryIds: [],
          permissionKinds: ['workspace_read'],
          workspaceId: catalogWorkspace.id,
        }),
      },
      expect.objectContaining({ onSuccess: expect.any(Function), onError: expect.any(Function) }),
    )
    const reveal = await screen.findByRole('dialog', { name: 'Token Created' })
    expect(within(reveal).getByText('plaintext-token-value')).toBeInTheDocument()
    await user.click(within(reveal).getByRole('button', { name: 'Copy' }))
    expect(writeText).toHaveBeenCalledWith('plaintext-token-value')
    expect(toast.success).toHaveBeenCalledWith('Copied')
    await user.click(within(reveal).getByRole('button', { name: 'Done' }))
    await waitFor(() => {
      expect(screen.queryByRole('dialog', { name: 'Token Created' })).not.toBeInTheDocument()
    })
  })

  it('warns when the mint library picker is partial', async () => {
    const user = userEvent.setup()
    mocks.librariesTotal = 9
    renderAccess()
    await user.click(await screen.findByRole('button', { name: 'Create token' }))
    const dialog = await screen.findByRole('dialog', { name: 'Create token' })
    await user.selectOptions(within(dialog).getByDisplayValue('Workspace'), 'library')

    expect(await within(dialog).findByText(/Showing 1 of 9 libraries/)).toBeInTheDocument()
  })

  it('omits expiresAt when minting a never-expiring token', async () => {
    const user = userEvent.setup()
    renderAccess()
    await user.click(await screen.findByRole('button', { name: 'Create token' }))
    const dialog = await screen.findByRole('dialog', { name: 'Create token' })
    await user.type(
      within(dialog).getByText('Label').parentElement!.querySelectorAll('input')[0]!,
      'Forever',
    )
    await user.selectOptions(within(dialog).getByDisplayValue('90 days'), 'never')
    await user.click(within(dialog).getByText('Workspace read'))
    await user.click(within(dialog).getByRole('button', { name: 'Create token' }))

    const body = mocks.mintToken.mock.calls[0]?.[0]?.body as Record<string, unknown>
    expect(body).toMatchObject({
      label: 'Forever',
      libraryIds: [],
      permissionKinds: ['workspace_read'],
      workspaceId: catalogWorkspace.id,
    })
    expect(body).not.toHaveProperty('expiresAt')
  })

  it('explains a failed mint', async () => {
    const user = userEvent.setup()
    mocks.mintToken.mockImplementation((_variables, options) => {
      options?.onError?.(new Error('mint failed'))
    })
    renderAccess()
    await user.click(await screen.findByRole('button', { name: 'Create token' }))
    const dialog = await screen.findByRole('dialog', { name: 'Create token' })
    await user.type(
      within(dialog).getByText('Label').parentElement!.querySelectorAll('input')[0]!,
      'CI token',
    )
    await user.click(within(dialog).getByText('Workspace read'))
    await user.click(within(dialog).getByRole('button', { name: 'Create token' }))

    expect(toast.error).toHaveBeenCalledWith('mint failed')
    expect(screen.getByRole('dialog', { name: 'Create token' })).toBeInTheDocument()
  })
})
