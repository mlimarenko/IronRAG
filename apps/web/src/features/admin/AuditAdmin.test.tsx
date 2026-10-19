import { act, render, screen, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { createMemoryRouter, RouterProvider } from 'react-router-dom'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import i18n from '@/shared/i18n'
import type { AuditGridColumnId } from '@/shared/api'
import type { AuditEvent } from './audit/model'
import AuditAdmin from './AuditAdmin'

function auditEvent(overrides: Partial<AuditEvent> & Pick<AuditEvent, 'id'>): AuditEvent {
  return {
    actionKind: 'iam.session.login',
    actor: 'Ada Lovelace',
    actorPrincipalId: 'user-self',
    assistantCall: undefined,
    createdAt: '2026-08-09T10:00:00Z',
    internalMessage: null,
    libraryIds: [],
    redactedMessage: 'session login succeeded',
    requestId: 'request-1',
    resultKind: 'succeeded',
    subjectSummary: 'session:019fe763-64c7-7013-9f50-a09cee875cd6',
    surfaceKind: 'rest',
    traceId: 'trace-1',
    ...overrides,
  }
}

const loginEvent = auditEvent({ id: 'audit-login' })
const failureEvent = auditEvent({
  actor: 'Grace Hopper',
  actorPrincipalId: 'user-grace',
  id: 'audit-failure',
  redactedMessage: 'identity provider timeout',
  resultKind: 'failed',
  surfaceKind: 'mcp',
})

const mocks = vi.hoisted(() => {
  const state: {
    data: { items: AuditEvent[]; nextCursor: string | null; total: number }
    error: unknown
    isLoading: boolean
    isPlaceholderData: boolean
    latestScope: Record<string, unknown> | null
    latestCursor: string | null
  } = {
    data: { items: [], nextCursor: null, total: 0 },
    error: null,
    isLoading: false,
    isPlaceholderData: false,
    latestScope: null,
    latestCursor: null,
  }
  return { ...state, refetch: vi.fn() }
})

vi.mock('@/shared/contexts/app-context', () => ({
  useApp: () => ({
    libraries: [
      { id: 'library-1', name: 'Library One', workspaceId: 'workspace-1' },
      { id: 'library-2', name: 'Library Two', workspaceId: 'workspace-2' },
    ],
    locale: 'en',
    workspaces: [
      { id: 'workspace-1', name: 'Workspace One' },
      { id: 'workspace-2', name: 'Workspace Two' },
    ],
  }),
}))

vi.mock('./audit/api', () => ({
  useAuditEvents: (
    scope: Record<string, unknown>,
    cursor: string | null,
    _t: unknown,
    _locale: unknown,
  ) => {
    mocks.latestScope = scope
    mocks.latestCursor = cursor
    return {
      data: mocks.data,
      error: mocks.error,
      isLoading: mocks.isLoading,
      isPlaceholderData: mocks.isPlaceholderData,
      refetch: mocks.refetch,
    }
  },
  useAuditFacets: () => ({
    serverFacets: {
      page: (_columnId: AuditGridColumnId) => ({
        options:
          _columnId === 'surface'
            ? ['rest', 'mcp'].map((value) => ({
                key: `string:${value}`,
                value,
                label: value,
                count: 1,
              }))
            : [],
        hasMore: false,
        pending: false,
        error: false,
      }),
      onOpen: () => {},
      onSearch: () => {},
      onLoadMore: () => {},
    },
  }),
  useAuditSelectedValues: () => ({
    pending: false,
    resolveLabel: (columnId: string, key: string) =>
      columnId === 'actor' && key === 'string:actor-1' ? 'Ada Lovelace' : undefined,
    selectedValues: [],
  }),
}))

function renderAudit(search = '') {
  const router = createMemoryRouter([{ element: <AuditAdmin />, path: '/admin/audit' }], {
    initialEntries: [search ? `/admin/audit?${search}` : '/admin/audit'],
  })
  return { router, ...render(<RouterProvider router={router} />) }
}

describe('AuditAdmin', () => {
  beforeEach(async () => {
    vi.clearAllMocks()
    mocks.isLoading = false
    mocks.isPlaceholderData = false
    mocks.error = null
    mocks.data = { items: [loginEvent, failureEvent], nextCursor: null, total: 2 }
    mocks.latestScope = null
    mocks.latestCursor = null
    await i18n.changeLanguage('en')
  })

  it('shows the loading state', () => {
    mocks.isLoading = true
    mocks.data = { items: [], nextCursor: null, total: 0 }
    renderAudit()

    expect(screen.getByText('Loading')).toBeInTheDocument()
    expect(screen.queryByText('IAM Session Login')).not.toBeInTheDocument()
  })

  it('retries after a load failure', async () => {
    const user = userEvent.setup()
    mocks.error = new Error('audit backend failed')
    mocks.data = { items: [], nextCursor: null, total: 0 }
    renderAudit()

    expect(screen.getByRole('alert')).toHaveTextContent('audit backend failed')
    expect(screen.getByText('audit backend failed')).toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'Try again' }))
    expect(mocks.refetch).toHaveBeenCalledTimes(1)
  })

  it('lists events through the shared grid', () => {
    renderAudit()

    expect(screen.getAllByText('IAM Session Login').length).toBeGreaterThan(0)
    expect(screen.getByText('Ada Lovelace')).toBeInTheDocument()
    expect(screen.getByText('Grace Hopper')).toBeInTheDocument()
    expect(screen.getByText('identity provider timeout')).toBeInTheDocument()
  })
  it('preserves both surface filters when clicks precede a navigation render', async () => {
    const { router } = renderAudit('workspaceId=workspace-1')
    await userEvent
      .setup()
      .click(within(screen.getByRole('columnheader', { name: /Source/i })).getByRole('button'))
    const rest = screen.getByRole('menuitemcheckbox', { name: 'rest' })
    const mcp = screen.getByRole('menuitemcheckbox', { name: 'mcp' })
    act(() => {
      rest.click()
      mcp.click()
    })
    const params = new URLSearchParams(router.state.location.search)
    expect(JSON.parse(params.get('filters')!)).toEqual({ surface: ['string:rest', 'string:mcp'] })
    expect(params.get('workspaceId')).toBe('workspace-1')
    expect(mocks.latestScope?.filters).toEqual({ surfaceKind: ['rest', 'mcp'] })
    expect(rest).toHaveAttribute('aria-checked', 'true')
    expect(mcp).toHaveAttribute('aria-checked', 'true')
  })

  it('renders authorized assistant model, exact cost and call counts with unavailable fallbacks', () => {
    mocks.data = {
      items: [
        auditEvent({
          id: 'assistant-priced',
          assistantCall: {
            queryExecutionId: 'query-1',
            models: [{ providerKind: 'openai_compatible', modelName: 'atlas' }],
            totalCost: '9007199254740993.125',
            currencyCode: 'USD',
            providerCallCount: 2,
          },
        }),
        auditEvent({
          id: 'assistant-unavailable',
          assistantCall: {
            queryExecutionId: 'query-2',
            models: [],
            totalCost: null,
            providerCallCount: 1,
          },
        }),
      ],
      nextCursor: null,
      total: 2,
    }
    renderAudit()
    expect(screen.getByText('openai_compatible:atlas')).toBeInTheDocument()
    expect(screen.getByText(/USD 9,007,199,254,740,993\.125/)).toBeInTheDocument()
    expect(screen.getByText(/2 calls/)).toBeInTheDocument()
    expect(screen.getByText('No billed model')).toBeInTheDocument()
    expect(screen.getByText(/Cost unavailable/)).toBeInTheDocument()
    expect(screen.getByText(/1 call/)).toBeInTheDocument()
  })

  it('hides assistant details when inclusion is disabled, without exposing cached metadata', async () => {
    mocks.data = {
      items: [
        auditEvent({
          id: 'assistant-toggle',
          assistantCall: {
            queryExecutionId: 'query-1',
            models: [{ providerKind: 'openai_compatible', modelName: 'atlas' }],
            totalCost: '0.125',
            currencyCode: 'USD',
            providerCallCount: 2,
          },
        }),
      ],
      nextCursor: null,
      total: 1,
    }
    const user = userEvent.setup()
    renderAudit()
    expect(screen.getByText('openai_compatible:atlas')).toBeInTheDocument()
    await user.click(screen.getByText('Assistant model and cost'))
    expect(mocks.latestScope).toMatchObject({ includeAssistant: false })
    expect(screen.queryByText('openai_compatible:atlas')).not.toBeInTheDocument()
    expect(screen.queryByText(/USD 0\.125/)).not.toBeInTheDocument()
  })

  it('does not invent a currency when the assistant cost has no currency identity', () => {
    mocks.data = {
      items: [
        auditEvent({
          id: 'assistant-no-currency',
          assistantCall: {
            queryExecutionId: 'query-1',
            models: [],
            totalCost: '0.125',
            providerCallCount: 0,
          },
        }),
      ],
      nextCursor: null,
      total: 1,
    }
    renderAudit()
    expect(screen.getByText(/Cost unavailable/)).toBeInTheDocument()
    expect(screen.queryByText(/USD/)).not.toBeInTheDocument()
  })

  it('exposes a sort and value-filter menu on every data column', () => {
    renderAudit()

    for (const label of ['Actor', 'Action', 'Source', 'Result', 'Time']) {
      expect(
        screen.getByRole('button', { name: `Column ${label}: sort and filter` }),
      ).toBeInTheDocument()
    }
  })

  it('blocks old cursors and export while a replacement sort page is pending', async () => {
    const user = userEvent.setup()
    mocks.data.nextCursor = 'old-created-at-cursor'
    const { router } = renderAudit()
    const next = screen.getByRole('button', { name: 'Next' })
    await user.click(next)
    expect(mocks.latestCursor).toBe('old-created-at-cursor')

    await user.click(screen.getByRole('button', { name: 'Column Action: sort and filter' }))
    mocks.isPlaceholderData = true
    await user.click(screen.getByRole('menuitem', { name: /ascending/i }))
    expect(mocks.latestScope).toMatchObject({ sort: { columnId: 'action', direction: 'asc' } })
    expect(mocks.latestCursor).toBeNull()
    expect(next).toHaveAttribute('aria-disabled', 'true')
    await user.click(next)
    expect(mocks.latestCursor).toBeNull()
    expect(screen.getByRole('button', { name: /Export/ })).toBeDisabled()
    expect(screen.getByRole('table')).toHaveAttribute('aria-busy', 'true')

    mocks.isPlaceholderData = false
    mocks.data.nextCursor = 'new-action-cursor'
    await act(async () => {
      await router.navigate(`${router.state.location.pathname}${router.state.location.search}`, {
        replace: true,
      })
    })
    expect(next).not.toHaveAttribute('aria-disabled')
    await user.click(next)
    expect(mocks.latestCursor).toBe('new-action-cursor')
  })

  it('maps a deeplinked column filter and sort into the typed server scope', () => {
    renderAudit(
      'filters=%7B%22result%22%3A%5B%22string%3Afailed%22%5D%2C%22actor%22%3A%5B%22null%22%5D%7D&sort=actor%3Aasc',
    )

    expect(mocks.latestScope).toMatchObject({
      filters: { resultKind: ['failed'], actorNull: true },
      sort: { columnId: 'actor', direction: 'asc' },
    })
    expect(mocks.latestCursor).toBeNull()
  })

  it('forwards the workspace/library scope and the internal switch', async () => {
    const user = userEvent.setup()
    renderAudit('workspaceId=workspace-1')

    expect(mocks.latestScope).toMatchObject({ workspaceId: 'workspace-1' })

    await user.click(screen.getByText('Show internal details'))
    expect(mocks.latestScope).toMatchObject({ internal: true })
  })

  it('offers the export only when there is something to export', () => {
    renderAudit()
    expect(screen.getByRole('button', { name: /Export/ })).toBeEnabled()

    mocks.data = { items: [], nextCursor: null, total: 0 }
    renderAudit()
    expect(screen.getAllByRole('button', { name: /Export/ })[1]).toBeDisabled()
  })
})
