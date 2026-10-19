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
  UserResponse,
} from '@/shared/api/generated'
import type * as UsersApi from './api/users'
import type { UserAccessRevision } from './api/users'
import UsersAdmin from './UsersAdmin'

const ada: UserResponse = {
  authProviderKind: 'local',
  displayName: 'Ada Lovelace',
  email: 'ada@example.com',
  login: 'ada',
  principalId: 'user-self',
  role: 'admin',
}

const grace: UserResponse = {
  authProviderKind: 'local',
  displayName: 'Grace Hopper',
  email: 'grace@example.com',
  login: 'grace',
  principalId: 'user-grace',
  role: 'operator',
}

const workspace: CatalogWorkspaceResponse = {
  displayName: 'Workspace One',
  id: 'workspace-1',
  lifecycleState: 'active',
  slug: 'workspace-one',
}

const library = {
  displayName: 'Library One',
  id: 'library-1',
  slug: 'library-one',
  workspaceId: workspace.id,
} as CatalogLibraryResponse

const accessRevision: UserAccessRevision = {
  access: {
    libraries: [
      {
        displayName: library.displayName,
        grantId: 'grant-library-1',
        libraryId: library.id,
        permissionKind: 'library_read',
        workspaceId: workspace.id,
      },
    ],
    principalId: grace.principalId,
    workspaces: [
      {
        displayName: workspace.displayName,
        grantId: 'grant-workspace-1',
        permissionKind: 'workspace_read',
        workspaceId: workspace.id,
      },
    ],
  },
  etag: '"etag-1"',
}

const mocks = vi.hoisted(() => {
  const state: {
    accessState: 'loading' | 'error' | 'ready'
    librariesIncomplete: boolean
    users: UserResponse[]
  } = {
    accessState: 'ready',
    librariesIncomplete: false,
    users: [],
  }
  return {
    ...state,
    createUser: vi.fn(),
    deleteUser: vi.fn(),
    listCatalogLibraries: vi.fn(),
    listUsers: vi.fn(),
    listWorkspaces: vi.fn(),
    patchUser: vi.fn(),
    setUserAccess: vi.fn(),
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
    locale: 'en',
    user: { id: 'user-self' },
  }),
}))

vi.mock('@/shared/api', () => ({
  Catalog: {
    listCatalogLibraries: mocks.listCatalogLibraries,
  },
  queries: {
    listCatalogWorkspacesOptions: () => ({
      queryFn: mocks.listWorkspaces,
      queryKey: ['admin', 'catalog-workspaces'],
    }),
    listIamUsersOptions: () => ({
      queryFn: mocks.listUsers,
      queryKey: ['admin', 'iam-users'],
    }),
  },
}))

vi.mock('./api/users', async (importOriginal) => {
  const actual = await importOriginal<typeof UsersApi>()
  return {
    ...actual,
    useUserMutations: () => ({
      createUser: { isPending: false, mutate: mocks.createUser },
      deleteUser: { isPending: false, mutate: mocks.deleteUser },
      patchUser: { isPending: false, mutate: mocks.patchUser },
      setUserAccess: { isPending: false, mutate: mocks.setUserAccess },
    }),
    userAccessOptions: (userId: string) => ({
      queryFn: async () => {
        if (mocks.accessState === 'loading') return await new Promise(() => undefined)
        if (mocks.accessState === 'error') throw new Error('access failed')
        return accessRevision
      },
      queryKey: ['admin', 'user-access', userId],
    }),
  }
})

function renderUsers() {
  const queryClient = new QueryClient({
    defaultOptions: { mutations: { retry: false }, queries: { retry: false } },
  })
  const router = createMemoryRouter([{ element: <UsersAdmin />, path: '/admin/users' }], {
    initialEntries: ['/admin/users'],
  })
  return render(
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  )
}

async function openAccess(user: ReturnType<typeof userEvent.setup>) {
  await screen.findByText(grace.displayName)
  const row = screen.getByText(grace.displayName).closest('article')
  if (!row) throw new Error('Grace Hopper row did not render')
  await user.click(within(row).getByRole('button', { name: 'Manage access' }))
  return screen.findByRole('dialog', { name: `Manage access · ${grace.displayName}` })
}

async function fillCreateForm(user: ReturnType<typeof userEvent.setup>, dialog: HTMLElement) {
  await user.type(
    within(dialog).getByText('Login').parentElement!.querySelector('input')!,
    'new.user',
  )
  await user.type(
    within(dialog).getByText('Email').parentElement!.querySelector('input')!,
    'new@example.com',
  )
  await user.type(
    within(dialog).getByText('Display name').parentElement!.querySelector('input')!,
    'New User',
  )
  await user.type(
    within(dialog).getByText('Password').parentElement!.querySelector('input')!,
    'password1',
  )
}

describe('UsersAdmin', () => {
  beforeEach(async () => {
    vi.clearAllMocks()
    mocks.accessState = 'ready'
    mocks.librariesIncomplete = false
    mocks.users = [ada, grace]
    mocks.listUsers.mockImplementation(async () => mocks.users)
    mocks.listWorkspaces.mockResolvedValue([workspace])
    mocks.listCatalogLibraries.mockImplementation(
      async ({ query }: { query: { offset: number } }) => {
        if (mocks.librariesIncomplete) {
          if (query.offset === 0) {
            return {
              data: { items: [library], limit: 200, offset: 0, totalCount: 3 },
            }
          }
          return {
            data: { items: [], limit: 200, offset: query.offset, totalCount: 3 },
          }
        }
        return {
          data: { items: [library], limit: 200, offset: 0, totalCount: 1 },
        }
      },
    )
    mocks.createUser.mockImplementation((_variables, options) => {
      options?.onSuccess?.({ login: _variables.body.login })
    })
    mocks.patchUser.mockImplementation((_variables, options) => {
      options?.onSuccess?.(undefined)
    })
    mocks.deleteUser.mockImplementation((_variables, options) => {
      options?.onSuccess?.(undefined)
    })
    mocks.setUserAccess.mockImplementation((_variables, options) => {
      options?.onSuccess?.(undefined)
    })
    await i18n.changeLanguage('en')
  })

  it('shows the loading state while users are fetched', async () => {
    mocks.listUsers.mockImplementation(() => new Promise(() => undefined))
    renderUsers()

    expect(await screen.findByText('Loading')).toBeInTheDocument()
    expect(screen.queryByText(ada.displayName)).not.toBeInTheDocument()
  })

  it('retries after a users list failure', async () => {
    const user = userEvent.setup()
    mocks.listUsers.mockRejectedValueOnce(new Error('users failed'))
    renderUsers()

    expect(await screen.findByText('Failed to load users')).toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'Try again' }))

    expect(await screen.findByText(ada.displayName)).toBeInTheDocument()
    expect(mocks.listUsers).toHaveBeenCalledTimes(2)
  })

  it('shows the empty directory when no users exist', async () => {
    mocks.users = []
    renderUsers()

    expect(await screen.findByText('No users yet')).toBeInTheDocument()
  })

  it('lists users with self badge and initials', async () => {
    renderUsers()

    expect(await screen.findByText(ada.displayName)).toBeInTheDocument()
    expect(screen.getByText('This is you')).toBeInTheDocument()
    expect(screen.getByText('AL')).toBeInTheDocument()
    expect(screen.getByText(grace.displayName)).toBeInTheDocument()
    expect(screen.getByText('GH')).toBeInTheDocument()
    expect(screen.getByText(`${ada.login} · ${ada.email}`)).toBeInTheDocument()
  })

  it('locks role and delete controls on the current account', async () => {
    renderUsers()
    const row = (await screen.findByText(ada.displayName)).closest('article')
    if (!row) throw new Error('Ada Lovelace row did not render')

    expect(within(row).getByRole('combobox', { name: 'Change role' })).toBeDisabled()
    expect(within(row).getByRole('button', { name: 'Delete' })).toBeDisabled()
    expect(within(row).getByRole('combobox', { name: 'Change role' })).toHaveAttribute(
      'title',
      'You cannot change your own account.',
    )
  })

  it('shows no matches when the role filter has no users', async () => {
    const user = userEvent.setup()
    renderUsers()
    await screen.findByText(grace.displayName)

    await user.selectOptions(screen.getByDisplayValue('All'), 'viewer')

    expect(await screen.findByText('No matches')).toBeInTheDocument()
    expect(screen.queryByText(grace.displayName)).not.toBeInTheDocument()
  })

  it('filters the list by a search for grace', async () => {
    const user = userEvent.setup()
    renderUsers()
    await screen.findByText(ada.displayName)

    await user.type(screen.getByPlaceholderText('Search'), 'grace')

    expect(screen.getByText(grace.displayName)).toBeInTheDocument()
    expect(screen.queryByText(ada.displayName)).not.toBeInTheDocument()
  })

  it('changes another user role and toasts success', async () => {
    const user = userEvent.setup()
    renderUsers()
    const row = (await screen.findByText(grace.displayName)).closest('article')
    if (!row) throw new Error('Grace Hopper row did not render')

    await user.selectOptions(within(row).getByRole('combobox', { name: 'Change role' }), 'admin')

    expect(mocks.patchUser).toHaveBeenCalledWith(
      { body: { role: 'admin' }, path: { userId: grace.principalId } },
      expect.objectContaining({ onSuccess: expect.any(Function), onError: expect.any(Function) }),
    )
    expect(toast.success).toHaveBeenCalledWith('Role updated for grace')
  })

  it('explains a 409 when changing role would remove the last admin', async () => {
    const user = userEvent.setup()
    mocks.patchUser.mockImplementation((_variables, options) => {
      options?.onError?.({ status: 409 })
    })
    renderUsers()
    const row = (await screen.findByText(grace.displayName)).closest('article')
    if (!row) throw new Error('Grace Hopper row did not render')

    await user.selectOptions(within(row).getByRole('combobox', { name: 'Change role' }), 'admin')

    expect(toast.error).toHaveBeenCalledWith('The final administrator cannot be removed.')
  })

  it('explains a failed role change that is not a 409', async () => {
    const user = userEvent.setup()
    mocks.patchUser.mockImplementation((_variables, options) => {
      options?.onError?.(new Error('role backend failed'))
    })
    renderUsers()
    const row = (await screen.findByText(grace.displayName)).closest('article')
    if (!row) throw new Error('Grace Hopper row did not render')

    await user.selectOptions(within(row).getByRole('combobox', { name: 'Change role' }), 'viewer')

    expect(toast.error).toHaveBeenCalledWith('role backend failed')
  })

  it('validates the create dialog and keeps Create disabled', async () => {
    const user = userEvent.setup()
    renderUsers()
    await user.click(await screen.findByRole('button', { name: 'Create' }))
    const dialog = screen.getByRole('dialog', { name: 'Create user' })

    await user.type(within(dialog).getByText('Login').parentElement!.querySelector('input')!, 'Ada')
    expect(within(dialog).getByText(/lowercase letters, numbers/)).toBeInTheDocument()
    expect(within(dialog).getByRole('button', { name: 'Create' })).toBeDisabled()
    expect(mocks.createUser).not.toHaveBeenCalled()
  })

  it('creates a user after the form is valid', async () => {
    const user = userEvent.setup()
    renderUsers()
    await user.click(await screen.findByRole('button', { name: 'Create' }))
    const dialog = screen.getByRole('dialog', { name: 'Create user' })
    await fillCreateForm(user, dialog)
    await user.click(within(dialog).getByRole('button', { name: 'Create' }))

    expect(mocks.createUser).toHaveBeenCalledWith(
      {
        body: {
          displayName: 'New User',
          email: 'new@example.com',
          login: 'new.user',
          password: 'password1',
          role: 'viewer',
        },
      },
      expect.objectContaining({ onSuccess: expect.any(Function), onError: expect.any(Function) }),
    )
    expect(toast.success).toHaveBeenCalledWith('User new.user created')
    await waitFor(() => {
      expect(screen.queryByRole('dialog', { name: 'Create user' })).not.toBeInTheDocument()
    })
  })

  it('keeps the create dialog open after a failed save', async () => {
    const user = userEvent.setup()
    mocks.createUser.mockImplementation((_variables, options) => {
      options?.onError?.(new Error('create backend failed'))
    })
    renderUsers()
    await user.click(await screen.findByRole('button', { name: 'Create' }))
    const dialog = screen.getByRole('dialog', { name: 'Create user' })
    await fillCreateForm(user, dialog)
    await user.click(within(dialog).getByRole('button', { name: 'Create' }))

    expect(toast.error).toHaveBeenCalledWith('create backend failed')
    expect(screen.getByRole('dialog', { name: 'Create user' })).toBeInTheDocument()
  })

  it('closes the create dialog without mutating when cancelled', async () => {
    const user = userEvent.setup()
    renderUsers()
    await user.click(await screen.findByRole('button', { name: 'Create' }))
    const dialog = screen.getByRole('dialog', { name: 'Create user' })
    await user.type(
      within(dialog).getByText('Login').parentElement!.querySelector('input')!,
      'draft',
    )
    await user.click(within(dialog).getByRole('button', { name: 'Cancel' }))

    await waitFor(() => {
      expect(screen.queryByRole('dialog', { name: 'Create user' })).not.toBeInTheDocument()
    })
    expect(mocks.createUser).not.toHaveBeenCalled()
  })

  it('deletes another user after confirmation', async () => {
    const user = userEvent.setup()
    renderUsers()
    const row = (await screen.findByText(grace.displayName)).closest('article')
    if (!row) throw new Error('Grace Hopper row did not render')
    await user.click(within(row).getByRole('button', { name: 'Delete' }))
    const dialog = await screen.findByRole('dialog', { name: 'Delete user' })
    expect(within(dialog).getByText(/revoke their sessions/)).toBeInTheDocument()

    await user.click(within(dialog).getByRole('button', { name: 'Delete' }))

    expect(mocks.deleteUser).toHaveBeenCalledWith(
      { path: { userId: grace.principalId } },
      expect.objectContaining({ onSuccess: expect.any(Function), onError: expect.any(Function) }),
    )
    expect(toast.success).toHaveBeenCalledWith('User grace deleted.')
  })

  it('explains a 409 when deleting the last administrator', async () => {
    const user = userEvent.setup()
    mocks.deleteUser.mockImplementation((_variables, options) => {
      options?.onError?.({ status: 409 })
    })
    renderUsers()
    const row = (await screen.findByText(grace.displayName)).closest('article')
    if (!row) throw new Error('Grace Hopper row did not render')
    await user.click(within(row).getByRole('button', { name: 'Delete' }))
    const dialog = await screen.findByRole('dialog', { name: 'Delete user' })
    await user.click(within(dialog).getByRole('button', { name: 'Delete' }))

    expect(toast.error).toHaveBeenCalledWith('The final administrator cannot be removed.')
    expect(screen.getByRole('dialog', { name: 'Delete user' })).toBeInTheDocument()
  })

  it('shows access loading inside the manage-access dialog', async () => {
    const user = userEvent.setup()
    mocks.accessState = 'loading'
    renderUsers()
    const dialog = await openAccess(user)

    expect(await within(dialog).findByText('Loading access...')).toBeInTheDocument()
    expect(within(dialog).getByRole('button', { name: 'Save' })).toBeDisabled()
  })

  it('retries when access loading fails', async () => {
    const user = userEvent.setup()
    mocks.accessState = 'error'
    renderUsers()
    const dialog = await openAccess(user)

    expect(await within(dialog).findByText('Failed to load access')).toBeInTheDocument()
    mocks.accessState = 'ready'
    await user.click(within(dialog).getByRole('button', { name: 'Try again' }))
    expect(await within(dialog).findByText(workspace.displayName)).toBeInTheDocument()
  })

  it('renders the access matrix when loading succeeds', async () => {
    const user = userEvent.setup()
    renderUsers()
    const dialog = await openAccess(user)

    expect(await within(dialog).findByText(workspace.displayName)).toBeInTheDocument()
    expect(
      within(dialog).getByPlaceholderText('Search workspaces and libraries'),
    ).toBeInTheDocument()
    expect(within(dialog).getByRole('button', { name: 'Save' })).toBeEnabled()
  })

  it('expands a workspace to show its libraries', async () => {
    const user = userEvent.setup()
    renderUsers()
    const dialog = await openAccess(user)
    await within(dialog).findByText(workspace.displayName)

    expect(within(dialog).queryByText(library.displayName)).not.toBeInTheDocument()
    await user.click(within(dialog).getByRole('button', { name: `${workspace.displayName}1` }))
    expect(within(dialog).getByText(library.displayName)).toBeInTheDocument()
  })

  it('filters access resources by search', async () => {
    const user = userEvent.setup()
    renderUsers()
    const dialog = await openAccess(user)
    await within(dialog).findByText(workspace.displayName)

    await user.type(
      within(dialog).getByPlaceholderText('Search workspaces and libraries'),
      'library one',
    )

    expect(within(dialog).getByText(library.displayName)).toBeInTheDocument()
  })

  it('saves access grants', async () => {
    const user = userEvent.setup()
    renderUsers()
    const dialog = await openAccess(user)
    await within(dialog).findByText(workspace.displayName)
    await user.click(within(dialog).getByRole('button', { name: 'Save' }))

    expect(mocks.setUserAccess).toHaveBeenCalledWith(
      {
        body: {
          libraries: [{ libraryId: library.id, permissionKind: 'library_read' }],
          workspaces: [{ permissionKind: 'workspace_read', workspaceId: workspace.id }],
        },
        etag: '"etag-1"',
        userId: grace.principalId,
      },
      expect.objectContaining({ onSuccess: expect.any(Function), onError: expect.any(Function) }),
    )
    expect(toast.success).toHaveBeenCalledWith(`Access updated for ${grace.displayName}`)
  })

  it('reloads a stale access revision after a 409', async () => {
    const user = userEvent.setup()
    mocks.setUserAccess.mockImplementation((_variables, options) => {
      options?.onError?.({ status: 409 })
    })
    renderUsers()
    const dialog = await openAccess(user)
    await within(dialog).findByText(workspace.displayName)
    await user.click(within(dialog).getByRole('button', { name: 'Save' }))

    expect(await within(dialog).findByText(/Access changed elsewhere/)).toBeInTheDocument()
    await user.click(within(dialog).getByRole('button', { name: 'Reload access' }))
    expect(within(dialog).queryByText(/Access changed elsewhere/)).not.toBeInTheDocument()
  })

  it('disables save when the library list is incomplete', async () => {
    const user = userEvent.setup()
    mocks.librariesIncomplete = true
    renderUsers()
    const dialog = await openAccess(user)

    expect(
      await within(dialog).findByText(/complete library list could not be loaded/),
    ).toBeInTheDocument()
    expect(within(dialog).getByRole('button', { name: 'Save' })).toBeDisabled()
  })

  it('closes the access dialog from Cancel', async () => {
    const user = userEvent.setup()
    renderUsers()
    const dialog = await openAccess(user)
    await within(dialog).findByText(workspace.displayName)
    await user.click(within(dialog).getByRole('button', { name: 'Cancel' }))

    await waitFor(() => {
      expect(
        screen.queryByRole('dialog', { name: `Manage access · ${grace.displayName}` }),
      ).not.toBeInTheDocument()
    })
    expect(mocks.setUserAccess).not.toHaveBeenCalled()
  })
})
