import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { render, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { createMemoryRouter, RouterProvider } from 'react-router-dom'
import { toast } from 'sonner'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import i18n from '@/shared/i18n'
import type {
  CatalogLibraryResponse,
  DeliveryAttemptResponse,
  SubscriptionResponse,
} from '@/shared/api/generated'
import WebhooksAdmin from './WebhooksAdmin'

const workspace = { id: 'workspace-1', name: 'Workspace One' }

const hookLibrary = {
  displayName: 'Library One',
  id: 'library-1',
  slug: 'library-one',
  workspaceId: workspace.id,
} as CatalogLibraryResponse

const activeHook: SubscriptionResponse = {
  active: true,
  createdAt: '2026-08-01T00:00:00Z',
  displayName: 'Ready notifier',
  eventTypes: ['revision.ready'],
  id: 'hook-active',
  targetUrl: 'https://hooks.example/ready',
  updatedAt: '2026-08-01T00:00:00Z',
  workspaceId: workspace.id,
}

const pausedHook: SubscriptionResponse = {
  active: false,
  createdAt: '2026-08-02T00:00:00Z',
  displayName: 'Delete notifier',
  eventTypes: ['document.deleted'],
  id: 'hook-paused',
  libraryId: hookLibrary.id,
  targetUrl: 'https://hooks.example/delete',
  updatedAt: '2026-08-02T00:00:00Z',
  workspaceId: workspace.id,
}

const delivery: DeliveryAttemptResponse = {
  attemptNumber: 2,
  createdAt: '2026-08-03T00:00:00Z',
  deliveryState: 'failed',
  errorMessage: 'connection reset',
  eventId: 'event-1',
  eventType: 'revision.ready',
  id: 'delivery-1',
  nextAttemptAt: '2026-08-03T01:00:00Z',
  responseStatus: 500,
  subscriptionId: activeHook.id,
  targetUrl: activeHook.targetUrl,
}

const secret = 's'.repeat(32)

const mocks = vi.hoisted(() => {
  const state: {
    appWorkspace: { id: string; name: string } | null
    deliveries: DeliveryAttemptResponse[]
    deliveriesState: 'loading' | 'error' | 'ready' | 'empty'
    detail: SubscriptionResponse | null
    detailState: 'loading' | 'error' | 'ready'
    librariesTotal: number
    subscriptions: SubscriptionResponse[]
    subscriptionsState: 'loading' | 'error' | 'ready'
  } = {
    appWorkspace: {
      id: 'workspace-1',
      name: 'Workspace One',
    },
    deliveries: [],
    deliveriesState: 'ready',
    detail: null,
    detailState: 'ready',
    librariesTotal: 1,
    subscriptions: [],
    subscriptionsState: 'ready',
  }
  return {
    ...state,
    createWebhook: vi.fn(),
    deleteWebhook: vi.fn(),
    updateWebhook: vi.fn(),
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
    activeWorkspace: mocks.appWorkspace,
    locale: 'en',
  }),
}))

vi.mock('./api/webhooks', () => ({
  useWebhookMutations: () => ({
    createWebhook: { isPending: false, mutate: mocks.createWebhook },
    deleteWebhook: { isPending: false, mutate: mocks.deleteWebhook },
    updateWebhook: { isPending: false, mutate: mocks.updateWebhook },
  }),
}))

vi.mock('@/shared/api', () => ({
  queries: {
    getWebhookSubscriptionOptions: ({ path }: { path: { subscriptionId: string } }) => ({
      queryFn: async () => {
        if (mocks.detailState === 'loading') return await new Promise(() => undefined)
        if (mocks.detailState === 'error') throw new Error('detail failed')
        return mocks.detail ?? mocks.subscriptions.find((hook) => hook.id === path.subscriptionId)
      },
      queryKey: ['admin', 'webhook-detail', path.subscriptionId],
    }),
    listCatalogLibrariesOptions: () => ({
      queryFn: async () => ({
        items: [hookLibrary],
        limit: 200,
        offset: 0,
        totalCount: mocks.librariesTotal,
      }),
      queryKey: ['admin', 'webhook-libraries'],
    }),
    listWebhookDeliveryAttemptsOptions: () => ({
      queryFn: async () => {
        if (mocks.deliveriesState === 'loading') return await new Promise(() => undefined)
        if (mocks.deliveriesState === 'error') throw new Error('deliveries failed')
        return { items: mocks.deliveriesState === 'empty' ? [] : mocks.deliveries }
      },
      queryKey: ['admin', 'webhook-deliveries'],
    }),
    listWebhookSubscriptionsOptions: () => ({
      queryFn: async () => {
        if (mocks.subscriptionsState === 'loading') return await new Promise(() => undefined)
        if (mocks.subscriptionsState === 'error') throw new Error('webhooks failed')
        return { items: mocks.subscriptions }
      },
      queryKey: ['admin', 'webhook-subscriptions'],
    }),
  },
}))

function renderWebhooks() {
  const queryClient = new QueryClient({
    defaultOptions: { mutations: { retry: false }, queries: { retry: false } },
  })
  const router = createMemoryRouter([{ element: <WebhooksAdmin />, path: '/admin/webhooks' }], {
    initialEntries: ['/admin/webhooks'],
  })
  return render(
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  )
}

async function fillCreateForm(
  user: ReturnType<typeof userEvent.setup>,
  dialog: HTMLElement,
  name = 'New hook',
) {
  await user.type(within(dialog).getByText('Name').parentElement!.querySelector('input')!, name)
  await user.type(
    within(dialog).getByText('Target URL').parentElement!.querySelector('input')!,
    'https://hooks.example/new',
  )
  await user.click(within(dialog).getByRole('checkbox', { name: 'Revision ready' }))
  await user.type(within(dialog).getByPlaceholderText('At least 32 bytes'), secret)
}

describe('WebhooksAdmin', () => {
  beforeEach(async () => {
    vi.clearAllMocks()
    mocks.appWorkspace = { ...workspace }
    mocks.subscriptions = [activeHook, pausedHook]
    mocks.subscriptionsState = 'ready'
    mocks.detail = activeHook
    mocks.detailState = 'ready'
    mocks.deliveries = [delivery]
    mocks.deliveriesState = 'ready'
    mocks.librariesTotal = 1
    mocks.createWebhook.mockImplementation((_variables, options) => options?.onSuccess?.({}))
    mocks.updateWebhook.mockImplementation((_variables, options) => options?.onSuccess?.({}))
    mocks.deleteWebhook.mockImplementation((_variables, options) => options?.onSuccess?.({}))
    await i18n.changeLanguage('en')
  })

  it('requires an active workspace', () => {
    mocks.appWorkspace = null
    renderWebhooks()

    expect(screen.getByText('Select an active workspace to manage webhooks.')).toBeInTheDocument()
  })

  it('shows the loading state', async () => {
    mocks.subscriptionsState = 'loading'
    renderWebhooks()

    expect(await screen.findByText('Loading')).toBeInTheDocument()
  })

  it('shows a load error', async () => {
    mocks.subscriptionsState = 'error'
    renderWebhooks()

    expect(await screen.findByText('Failed to load webhook subscriptions.')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Try again' })).toBeInTheDocument()
  })

  it('shows the empty workspace state', async () => {
    mocks.subscriptions = []
    renderWebhooks()

    expect(await screen.findByText('No webhooks yet')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Create webhook' })).toBeInTheDocument()
  })

  it('lists webhook cards', async () => {
    renderWebhooks()

    expect(await screen.findByText(activeHook.displayName)).toBeInTheDocument()
    expect(screen.getByText(pausedHook.displayName)).toBeInTheDocument()
    expect(screen.getByText(activeHook.targetUrl)).toBeInTheDocument()
    expect(screen.getByText('Revision ready')).toBeInTheDocument()
    expect(screen.getByText('Workspace One')).toBeInTheDocument()
    expect(screen.getByText('Library')).toBeInTheDocument()
  })

  it('filters the list by search', async () => {
    const user = userEvent.setup()
    renderWebhooks()
    await screen.findByText(activeHook.displayName)

    await user.type(screen.getByPlaceholderText('Search'), 'delete')

    expect(screen.getByText(pausedHook.displayName)).toBeInTheDocument()
    expect(screen.queryByText(activeHook.displayName)).not.toBeInTheDocument()
  })

  it('filters the list by paused status', async () => {
    const user = userEvent.setup()
    renderWebhooks()
    await screen.findByText(activeHook.displayName)

    await user.selectOptions(screen.getByDisplayValue('All'), 'paused')

    expect(screen.getByText(pausedHook.displayName)).toBeInTheDocument()
    expect(screen.queryByText(activeHook.displayName)).not.toBeInTheDocument()
  })

  it('keeps Save disabled until the create form is valid', async () => {
    const user = userEvent.setup()
    renderWebhooks()
    await user.click(await screen.findByRole('button', { name: 'Create' }))
    const dialog = await screen.findByRole('dialog', { name: 'Create' })

    expect(within(dialog).getByRole('button', { name: 'Save' })).toBeDisabled()
    await user.type(within(dialog).getByText('Name').parentElement!.querySelector('input')!, 'x')
    expect(within(dialog).getByRole('button', { name: 'Save' })).toBeDisabled()
    expect(mocks.createWebhook).not.toHaveBeenCalled()
  })

  it('creates a workspace-scoped webhook', async () => {
    const user = userEvent.setup()
    renderWebhooks()
    await user.click(await screen.findByRole('button', { name: 'Create' }))
    const dialog = await screen.findByRole('dialog', { name: 'Create' })
    await fillCreateForm(user, dialog)
    await user.click(within(dialog).getByRole('button', { name: 'Save' }))

    expect(mocks.createWebhook).toHaveBeenCalledWith(
      {
        body: {
          displayName: 'New hook',
          eventTypes: ['revision.ready'],
          secret,
          targetUrl: 'https://hooks.example/new',
        },
        workspaceId: workspace.id,
      },
      expect.objectContaining({ onSuccess: expect.any(Function), onError: expect.any(Function) }),
    )
    expect(toast.success).toHaveBeenCalledWith('Webhook subscription created.')
    await waitFor(() => {
      expect(screen.queryByRole('dialog', { name: 'Create' })).not.toBeInTheDocument()
    })
  })

  it('creates a library-scoped webhook with custom headers', async () => {
    const user = userEvent.setup()
    renderWebhooks()
    await user.click(await screen.findByRole('button', { name: 'Create' }))
    const dialog = await screen.findByRole('dialog', { name: 'Create' })
    await fillCreateForm(user, dialog, 'Library hook')
    await user.selectOptions(within(dialog).getByDisplayValue('Entire workspace'), 'library')
    await user.selectOptions(within(dialog).getByDisplayValue('Select a library'), hookLibrary.id)
    await user.click(within(dialog).getByRole('button', { name: 'Add' }))
    await user.type(within(dialog).getByLabelText('Header name'), 'X-Source')
    await user.type(within(dialog).getByLabelText('Header value'), 'ironrag')
    await user.click(within(dialog).getByRole('button', { name: 'Save' }))

    expect(mocks.createWebhook).toHaveBeenCalledWith(
      {
        body: {
          customHeaders: { 'X-Source': 'ironrag' },
          displayName: 'Library hook',
          eventTypes: ['revision.ready'],
          libraryId: hookLibrary.id,
          secret,
          targetUrl: 'https://hooks.example/new',
        },
        workspaceId: workspace.id,
      },
      expect.objectContaining({ onSuccess: expect.any(Function) }),
    )
  })

  it('warns when the library picker is partial', async () => {
    const user = userEvent.setup()
    mocks.librariesTotal = 12
    renderWebhooks()
    await user.click(await screen.findByRole('button', { name: 'Create' }))
    const dialog = await screen.findByRole('dialog', { name: 'Create' })
    await user.selectOptions(within(dialog).getByDisplayValue('Entire workspace'), 'library')

    expect(await within(dialog).findByText(/Showing 1 of 12 libraries/)).toBeInTheDocument()
  })

  it('keeps the create dialog open after an error', async () => {
    const user = userEvent.setup()
    mocks.createWebhook.mockImplementation((_variables, options) => {
      options?.onError?.(new Error('create webhook failed'))
    })
    renderWebhooks()
    await user.click(await screen.findByRole('button', { name: 'Create' }))
    const dialog = await screen.findByRole('dialog', { name: 'Create' })
    await fillCreateForm(user, dialog)
    await user.click(within(dialog).getByRole('button', { name: 'Save' }))

    expect(toast.error).toHaveBeenCalledWith('create webhook failed')
    expect(screen.getByRole('dialog', { name: 'Create' })).toBeInTheDocument()
  })

  it('shows edit loading state', async () => {
    const user = userEvent.setup()
    mocks.detailState = 'loading'
    renderWebhooks()
    const card = (await screen.findByText(activeHook.displayName)).closest('.webhook-card')
    if (!card) throw new Error('active webhook card did not render')
    await user.click(within(card as HTMLElement).getByRole('button', { name: 'Edit' }))

    const dialog = await screen.findByRole('dialog', { name: 'Edit' })
    expect(within(dialog).getByText('Loading')).toBeInTheDocument()
    expect(within(dialog).getByRole('button', { name: 'Save' })).toBeDisabled()
  })

  it('retries when webhook details fail to load', async () => {
    const user = userEvent.setup()
    mocks.detailState = 'error'
    renderWebhooks()
    const card = (await screen.findByText(activeHook.displayName)).closest('.webhook-card')
    if (!card) throw new Error('active webhook card did not render')
    await user.click(within(card as HTMLElement).getByRole('button', { name: 'Edit' }))
    const dialog = await screen.findByRole('dialog', { name: 'Edit' })

    expect(await within(dialog).findByText('Failed to load webhook details.')).toBeInTheDocument()
    mocks.detailState = 'ready'
    await user.click(within(dialog).getByRole('button', { name: 'Try again' }))
    expect(await within(dialog).findByDisplayValue(activeHook.displayName)).toBeInTheDocument()
  })

  it('updates an existing webhook', async () => {
    const user = userEvent.setup()
    renderWebhooks()
    const card = (await screen.findByText(activeHook.displayName)).closest('.webhook-card')
    if (!card) throw new Error('active webhook card did not render')
    await user.click(within(card as HTMLElement).getByRole('button', { name: 'Edit' }))
    const dialog = await screen.findByRole('dialog', { name: 'Edit' })
    const name = await within(dialog).findByDisplayValue(activeHook.displayName)
    await user.clear(name)
    await user.type(name, 'Renamed hook')
    await user.click(within(dialog).getByRole('button', { name: 'Save' }))

    expect(mocks.updateWebhook).toHaveBeenCalledWith(
      {
        body: {
          active: true,
          displayName: 'Renamed hook',
          eventTypes: ['revision.ready'],
          targetUrl: activeHook.targetUrl,
        },
        subscriptionId: activeHook.id,
        workspaceId: workspace.id,
      },
      expect.objectContaining({ onSuccess: expect.any(Function), onError: expect.any(Function) }),
    )
    expect(toast.success).toHaveBeenCalledWith('Webhook subscription updated.')
  })

  async function openDeliveries(user: ReturnType<typeof userEvent.setup>) {
    const card = (await screen.findByText(activeHook.displayName)).closest('.webhook-card')
    if (!card) throw new Error('active webhook card did not render')
    await user.click(within(card as HTMLElement).getByRole('button', { name: 'Delivery attempts' }))
    return screen.findByRole('dialog', {
      name: `Delivery attempts · ${activeHook.displayName}`,
    })
  }

  it('shows deliveries loading', async () => {
    const user = userEvent.setup()
    mocks.deliveriesState = 'loading'
    renderWebhooks()
    expect(within(await openDeliveries(user)).getByText('Loading')).toBeInTheDocument()
  })

  it('shows a deliveries load error', async () => {
    const user = userEvent.setup()
    mocks.deliveriesState = 'error'
    renderWebhooks()
    expect(await openDeliveries(user)).toHaveTextContent('Failed to load delivery attempts.')
  })

  it('shows the empty deliveries state', async () => {
    const user = userEvent.setup()
    mocks.deliveriesState = 'empty'
    renderWebhooks()
    expect(await openDeliveries(user)).toHaveTextContent('No delivery attempts match this filter.')
  })

  it('lists delivery attempts', async () => {
    const user = userEvent.setup()
    renderWebhooks()
    const drawer = await openDeliveries(user)
    expect(within(drawer).getByText('Revision ready')).toBeInTheDocument()
    expect(within(drawer).getByText('Attempt 2')).toBeInTheDocument()
    expect(within(drawer).getByText('HTTP 500')).toBeInTheDocument()
    expect(within(drawer).getByText(/connection reset/)).toBeInTheDocument()
  })

  it('cancels webhook deletion', async () => {
    const user = userEvent.setup()
    renderWebhooks()
    const card = (await screen.findByText(activeHook.displayName)).closest('.webhook-card')
    if (!card) throw new Error('active webhook card did not render')
    await user.click(within(card as HTMLElement).getByRole('button', { name: 'Delete' }))
    const dialog = await screen.findByRole('dialog', { name: 'Delete webhook subscription' })
    await user.click(within(dialog).getByRole('button', { name: 'Cancel' }))

    await waitFor(() => {
      expect(
        screen.queryByRole('dialog', { name: 'Delete webhook subscription' }),
      ).not.toBeInTheDocument()
    })
    expect(mocks.deleteWebhook).not.toHaveBeenCalled()
  })

  it('deletes a webhook', async () => {
    const user = userEvent.setup()
    renderWebhooks()
    const card = (await screen.findByText(activeHook.displayName)).closest('.webhook-card')
    if (!card) throw new Error('active webhook card did not render')
    await user.click(within(card as HTMLElement).getByRole('button', { name: 'Delete' }))
    const dialog = await screen.findByRole('dialog', { name: 'Delete webhook subscription' })
    await user.click(within(dialog).getByRole('button', { name: 'Delete' }))

    expect(mocks.deleteWebhook).toHaveBeenCalledWith(
      expect.objectContaining({
        onDraining: expect.any(Function),
        subscriptionId: activeHook.id,
        workspaceId: workspace.id,
      }),
      expect.objectContaining({ onSuccess: expect.any(Function), onError: expect.any(Function) }),
    )
    expect(toast.success).toHaveBeenCalledWith('Webhook subscription deleted.')
  })

  it('marks a webhook as draining when delete is deferred', async () => {
    const user = userEvent.setup()
    mocks.deleteWebhook.mockImplementation((variables) => {
      variables.onDraining()
    })
    renderWebhooks()
    const card = (await screen.findByText(activeHook.displayName)).closest('.webhook-card')
    if (!card) throw new Error('active webhook card did not render')
    await user.click(within(card as HTMLElement).getByRole('button', { name: 'Delete' }))
    const dialog = await screen.findByRole('dialog', { name: 'Delete webhook subscription' })
    await user.click(within(dialog).getByRole('button', { name: 'Delete' }))

    expect(toast.warning).toHaveBeenCalledWith('Draining deliveries')
    expect(within(card as HTMLElement).getByText('Draining deliveries')).toBeInTheDocument()
    expect(within(card as HTMLElement).getByRole('button', { name: 'Edit' })).toBeDisabled()
  })

  it('explains a failed delete', async () => {
    const user = userEvent.setup()
    mocks.deleteWebhook.mockImplementation((_variables, options) => {
      options?.onError?.(new Error('delete webhook failed'))
    })
    renderWebhooks()
    const card = (await screen.findByText(activeHook.displayName)).closest('.webhook-card')
    if (!card) throw new Error('active webhook card did not render')
    await user.click(within(card as HTMLElement).getByRole('button', { name: 'Delete' }))
    const dialog = await screen.findByRole('dialog', { name: 'Delete webhook subscription' })
    await user.click(within(dialog).getByRole('button', { name: 'Delete' }))

    expect(toast.error).toHaveBeenCalledWith('delete webhook failed')
  })
})
