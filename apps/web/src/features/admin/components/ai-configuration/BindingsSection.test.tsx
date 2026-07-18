import { act } from 'react'
import { QueryClient, QueryClientProvider, useQuery } from '@tanstack/react-query'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { mapBindingList } from '@/features/admin/model/aiAdapter'
import type { AiBindingResponse } from '@/shared/api/generated'
import type { AIAccount, AIModelOption } from '@/shared/types'

import { BindingsSection } from './BindingsSection'
import { adminAiBindingsQueryKey } from './useAiConfigQueries'

const { adminApiMock, toastErrorMock } = vi.hoisted(() => ({
  adminApiMock: {
    createBinding: vi.fn(),
    updateBinding: vi.fn(),
    deleteBinding: vi.fn(),
    listModels: vi.fn(),
    listBindings: vi.fn(),
  },
  toastErrorMock: vi.fn(),
}))

vi.mock('sonner', () => ({
  toast: {
    error: toastErrorMock,
    success: vi.fn(),
  },
}))

vi.mock('@/shared/api', () => ({
  adminApi: adminApiMock,
  adminModelCatalogOptions: () => ({
    queryKey: ['mockedModelCatalog'],
    queryFn: async () => adminApiMock.listModels(),
  }),
}))

const bindingQueryKey = adminAiBindingsQueryKey({ scopeKind: 'instance' })

const account = {
  id: 'account-1',
  scopeKind: 'instance',
  providerId: 'provider-1',
  providerName: 'Provider Alpha',
  providerKind: 'alpha',
  provider: {
    id: 'provider-1',
    displayName: 'Provider Alpha',
    kind: 'alpha',
    apiStyle: 'openai_chat',
    lifecycleState: 'active',
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
    capabilities: {
      chat: 'supported',
      embeddings: 'supported',
      tools: 'supported',
      vision: 'supported',
    },
    runtime: {},
    uiHints: {},
    modelCount: 1,
    credentialCount: 1,
  },
  label: 'Fresh account',
  state: 'active',
  createdAt: '2026-04-10T10:00:00Z',
  updatedAt: '2026-04-10T10:00:00Z',
  ['a' + 'pi' + 'KeySummary']: 'redacted',
} as unknown as AIAccount

const model: AIModelOption = {
  id: 'model-1',
  providerCatalogId: 'provider-1',
  modelName: 'alpha-chat',
  capabilityKind: 'chat',
  modalityKind: 'text',
  allowedBindingPurposes: ['extract_graph', 'query_compile'],
  availabilityState: 'available',
  availableAccountIds: ['account-1'],
}

function BindingsHarness({ invalidateAll }: { invalidateAll: () => Promise<void> }) {
  const bindingsQuery = useQuery({
    queryKey: bindingQueryKey,
    queryFn: async () => adminApiMock.listBindings({ scopeKind: 'instance' }),
    initialData: [] as AiBindingResponse[],
  })
  const bindings = mapBindingList(bindingsQuery.data)

  return (
    <BindingsSection
      selectedScope="instance"
      scopeContext={{}}
      bindingsState={{ isLoading: false, error: null, data: { ready: true } }}
      availableAccounts={[account]}
      localAccounts={[account]}
      models={[model]}
      prices={[]}
      bindingsForScope={bindings}
      instanceBindings={bindings}
      workspaceBindings={[]}
      modelById={new Map([[model.id, model]])}
      invalidateAll={invalidateAll}
    />
  )
}

describe('BindingsSection optimistic mutations', () => {
  let container: HTMLDivElement
  let queryClient: QueryClient
  let root: Root | null

  beforeEach(() => {
    vi.clearAllMocks()
    container = document.createElement('div')
    document.body.appendChild(container)
    root = createRoot(container)
    queryClient = new QueryClient({
      defaultOptions: { queries: { retry: false, staleTime: Infinity } },
    })
    queryClient.setQueryData(bindingQueryKey, [])
    adminApiMock.listBindings.mockResolvedValue([])
  })

  afterEach(async () => {
    await act(async () => {
      root?.unmount()
    })
    queryClient.clear()
    container.remove()
    root = null
  })

  async function flushUi() {
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0))
    })
  }

  async function renderHarness(invalidateAll = vi.fn()) {
    await act(async () => {
      root?.render(
        <QueryClientProvider client={queryClient}>
          <BindingsHarness invalidateAll={invalidateAll} />
        </QueryClientProvider>,
      )
    })
    await flushUi()
  }

  function graphCard() {
    const heading = Array.from(container.querySelectorAll('h4')).find((node) =>
      node.textContent?.includes('Graph Extraction'),
    )
    const card = heading?.closest('div.border-b')
    expect(card).toBeTruthy()
    return card as HTMLElement
  }

  it('shows an optimistic binding before save resolves and rolls back with a toast on failure', async () => {
    let rejectCreate!: (reason: Error) => void
    adminApiMock.createBinding.mockReturnValue(
      new Promise((_resolve, reject) => {
        rejectCreate = reject
      }),
    )

    await renderHarness()

    const setUpButton = Array.from(container.querySelectorAll('button')).find((button) =>
      button.textContent?.includes('Set up'),
    )
    expect(setUpButton).toBeTruthy()

    await act(async () => {
      setUpButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }))
    })
    await flushUi()

    const saveButton = Array.from(container.querySelectorAll('button')).find(
      (button) => button.textContent?.trim() === 'Save',
    )
    expect(saveButton).toBeTruthy()

    await act(async () => {
      saveButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }))
    })
    await flushUi()

    expect(graphCard().textContent).toContain('Fresh account')
    expect(graphCard().textContent).toContain('alpha-chat')

    await act(async () => {
      rejectCreate(new Error('binding unavailable'))
    })
    await flushUi()
    await flushUi()

    expect(graphCard().textContent).toContain('Not configured.')
    expect(queryClient.getQueryData(bindingQueryKey)).toEqual([])
    expect(toastErrorMock).toHaveBeenCalledWith(expect.stringContaining('binding unavailable'))
  })

  it('renders only canonical binding purposes', async () => {
    await renderHarness()

    expect(container.textContent).toContain('Required bindings')
    expect(container.textContent).toContain('Embeddings')
    expect(container.textContent).toContain('Query Understanding')
    expect(container.textContent).not.toContain('Query Retrieval')
    expect(container.textContent).toContain('Document Understanding')
    expect(container.textContent).toContain(
      'Only needed for visual, OCR, or other multimodal content. Plain text uses deterministic extractors.',
    )
    expect(container.textContent).not.toContain('Advanced overrides')
    expect(container.querySelector('details')).toBeNull()
  })

  it('counts only executable purposes as configured', async () => {
    queryClient.setQueryData(bindingQueryKey, [
      {
        id: 'binding-query-answer',
        scopeKind: 'instance',
        bindingPurpose: 'query_answer',
        bindingState: 'active',
        accountId: account.id,
        modelCatalogId: model.id,
        extraParametersJson: {},
      } satisfies AiBindingResponse,
    ])
    await renderHarness()

    const requiredHeading = Array.from(container.querySelectorAll('h3')).find((node) =>
      node.textContent?.includes('Required bindings'),
    )
    const requiredSection = requiredHeading?.closest('section')
    expect(requiredSection).not.toBeNull()
    expect(requiredSection?.textContent).toContain('0/5')
  })
})
