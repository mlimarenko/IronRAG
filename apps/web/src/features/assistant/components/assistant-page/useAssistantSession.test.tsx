import { act, renderHook, waitFor } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { ReactNode } from 'react'
import type { TFunction } from 'i18next'
import { afterEach, expect, it, vi } from 'vitest'
import { queryApi, queries } from '@/shared/api'
import type { AssistantHydratedConversation } from '@/shared/api/generated'
import type { AssistantTurnExecutionResponse } from '@/shared/api/query'
import { useAssistantSession } from './useAssistantSession'

const sessionId = 'session-1'
const executionId = '0192f0aa-0000-7000-8000-000000000003'
const session = {
  id: sessionId,
  libraryId: 'library-1',
  workspaceId: 'workspace-1',
  title: 'Q',
  conversationState: 'active',
  createdAt: '2026-05-13T00:00:00Z',
  updatedAt: '2026-05-13T00:00:00Z',
  turnCount: 2,
}
const oldHistory: AssistantHydratedConversation = {
  session,
  messages: [
    { id: 'q1', role: 'user', content: 'Q', timestamp: '2026-05-13T00:00:00Z' },
    {
      id: 'a1',
      role: 'assistant',
      content: 'A1',
      executionId: 'old',
      timestamp: '2026-05-13T00:00:01Z',
    },
  ],
}

afterEach(() => {
  vi.restoreAllMocks()
  vi.unstubAllGlobals()
  localStorage.clear()
})

it('keeps a repeated local question while an accepted stream recovers from a pre-frame drop', async () => {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity } },
  })
  const listKey = queries.listQuerySessionsOptions({ path: { libraryId: 'library-1' } }).queryKey
  const sessionKey = queries.getQuerySessionOptions({ path: { sessionId } }).queryKey
  client.setQueryData(listKey, { items: [session], nextCursor: null, total: 1 })
  client.setQueryData(sessionKey, oldHistory)
  localStorage.setItem(
    'ironrag.v1.assistant.active-session:workspace-1:library-1',
    JSON.stringify(sessionId),
  )
  let finish!: (value: AssistantTurnExecutionResponse) => void
  const recovery = new Promise<AssistantTurnExecutionResponse>((resolve) => {
    finish = resolve
  })
  const getExecution = vi.spyOn(queryApi, 'getExecution').mockReturnValue(recovery)
  const fetchMock = vi.fn().mockResolvedValue(
    new Response(
      new ReadableStream({
        pull() {
          throw new Error('stream dropped')
        },
      }),
      { status: 200, headers: { 'x-ironrag-query-execution-id': executionId } },
    ),
  )
  vi.stubGlobal('fetch', fetchMock)
  const wrapper = ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={client}>{children}</QueryClientProvider>
  )
  const { result } = renderHook(
    () =>
      useAssistantSession({
        workspaceId: 'workspace-1',
        libraryId: 'library-1',
        t: ((key: string) => key) as TFunction,
      }),
    { wrapper },
  )
  await waitFor(() => expect(result.current.messages.map((m) => m.content)).toEqual(['Q', 'A1']))
  const browserClock = vi.spyOn(Date, 'now').mockReturnValue(Date.parse('2026-05-13T05:59:00Z'))
  act(() => {
    expect(result.current.sendQuestion('Q')).toBe(true)
  })
  browserClock.mockRestore()
  await waitFor(() =>
    expect(getExecution).toHaveBeenCalledWith(executionId, expect.any(AbortSignal)),
  )
  act(() => {
    client.setQueryData(sessionKey, {
      ...oldHistory,
      messages: oldHistory.messages.map((message) => ({
        ...message,
        timestamp: '2026-05-13T00:00:02Z',
      })),
    })
  })
  await waitFor(() => expect(result.current.messages[0]?.timestamp).toBe('2026-05-13T00:00:02Z'))
  await waitFor(() =>
    expect(result.current.messages.map((m) => m.content)).toEqual(['Q', 'A1', 'Q', '']),
  )
  act(() => {
    client.setQueryData(sessionKey, {
      ...oldHistory,
      messages: [
        ...oldHistory.messages,
        { id: 'q2', role: 'user', content: 'Q', timestamp: '2026-05-13T00:00:03Z' },
        {
          id: executionId,
          role: 'assistant',
          content: '',
          executionId,
          elapsedMs: 100,
          timestamp: '2026-05-13T00:00:04Z',
        },
      ],
    })
  })
  await waitFor(() =>
    expect(result.current.messages.map((m) => m.id)).toEqual(['q1', 'a1', 'q2', executionId]),
  )
  const detail = {
    execution: {
      id: executionId,
      conversationId: sessionId,
      requestTurnId: 'q2',
      responseTurnId: 'a2',
      lifecycleState: 'completed',
    },
    requestTurn: { id: 'q2', conversationId: sessionId, turnKind: 'user', contentText: 'Q' },
    responseTurn: {
      id: 'a2',
      conversationId: sessionId,
      turnKind: 'assistant',
      executionId,
      contentText: 'A2',
    },
    runtimeSummary: {
      acceptedAt: '2026-05-13T13:00:00Z',
      completedAt: '2026-05-13T13:00:04Z',
    },
    chunkReferences: [],
    entityReferences: [],
    preparedSegmentReferences: [],
    relationReferences: [],
    technicalFactReferences: [],
    runtimeStageSummaries: [],
    verificationState: 'verified',
    verificationWarnings: [],
  } as unknown as AssistantTurnExecutionResponse
  await act(async () => {
    finish(detail)
  })
  await waitFor(() =>
    expect(result.current.messages.map((m) => m.content)).toEqual(['Q', 'A1', 'Q', 'A2']),
  )
  act(() => {
    client.setQueryData(sessionKey, {
      ...oldHistory,
      messages: oldHistory.messages.map((message) => ({
        ...message,
        timestamp: '2026-05-13T00:00:06Z',
      })),
    })
  })
  await waitFor(() => expect(result.current.messages[0]?.timestamp).toBe('2026-05-13T00:00:06Z'))
  expect(result.current.messages[2]).toMatchObject({
    id: 'q2',
    timestamp: '2026-05-13T13:00:00Z',
  })
  expect(result.current.messages[3]).toMatchObject({
    id: 'a2',
    timestamp: '2026-05-13T13:00:04Z',
  })
  act(() => {
    client.setQueryData<AssistantHydratedConversation>(sessionKey, {
      ...oldHistory,
      messages: [
        ...oldHistory.messages.map((message) => ({
          ...message,
          timestamp: '2026-05-13T00:00:05Z',
        })),
        { id: 'q2', role: 'user', content: 'Q', timestamp: '2026-05-13T13:00:00Z' },
        {
          id: executionId,
          role: 'assistant',
          content: '',
          executionId,
          elapsedMs: 100,
          timestamp: '2026-05-13T13:00:00Z',
        },
      ],
    })
  })
  await waitFor(() => expect(result.current.messages[0]?.timestamp).toBe('2026-05-13T00:00:05Z'))
  expect(result.current.messages.map((m) => [m.id, m.content])).toEqual([
    ['q1', 'Q'],
    ['a1', 'A1'],
    ['q2', 'Q'],
    ['a2', 'A2'],
  ])
  act(() => {
    client.setQueryData(sessionKey, {
      ...oldHistory,
      messages: [
        ...oldHistory.messages,
        { id: 'q2', role: 'user', content: 'Q', timestamp: '2026-05-13T00:00:03Z' },
        {
          id: 'a2',
          role: 'assistant',
          content: 'A2',
          executionId,
          timestamp: '2026-05-13T00:00:04Z',
        },
      ],
    })
  })
  await waitFor(() =>
    expect(result.current.messages.map((m) => m.id)).toEqual(['q1', 'a1', 'q2', 'a2']),
  )
  expect(fetchMock).toHaveBeenCalledTimes(1)
  expect(getExecution).toHaveBeenCalledTimes(1)
})

it('keeps streamed activity visible after a history refresh confirms the accepted turn', async () => {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity } },
  })
  const listKey = queries.listQuerySessionsOptions({ path: { libraryId: 'library-1' } }).queryKey
  const sessionKey = queries.getQuerySessionOptions({ path: { sessionId } }).queryKey
  client.setQueryData(listKey, { items: [session], nextCursor: null, total: 1 })
  client.setQueryData(sessionKey, oldHistory)
  localStorage.setItem(
    'ironrag.v1.assistant.active-session:workspace-1:library-1',
    JSON.stringify(sessionId),
  )
  let stream!: ReadableStreamDefaultController<Uint8Array>
  const fetchMock = vi.fn().mockResolvedValue(
    new Response(
      new ReadableStream({
        start(controller) {
          stream = controller
        },
      }),
      { status: 200, headers: { 'x-ironrag-query-execution-id': executionId } },
    ),
  )
  vi.stubGlobal('fetch', fetchMock)
  const wrapper = ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={client}>{children}</QueryClientProvider>
  )
  const { result } = renderHook(
    () =>
      useAssistantSession({
        workspaceId: 'workspace-1',
        libraryId: 'library-1',
        t: ((key: string) => key) as TFunction,
      }),
    { wrapper },
  )
  await waitFor(() => expect(result.current.messages.map((m) => m.content)).toEqual(['Q', 'A1']))
  act(() => {
    expect(result.current.sendQuestion('Q')).toBe(true)
  })
  await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(1))
  await waitFor(() =>
    expect(result.current.messages.map((m) => m.content)).toEqual(['Q', 'A1', 'Q', '']),
  )

  // The history poll confirms the accepted execution while the stream is still
  // open, so the optimistic placeholder gives way to the server message id.
  act(() => {
    client.setQueryData(sessionKey, {
      ...oldHistory,
      messages: [
        ...oldHistory.messages,
        { id: 'q2', role: 'user', content: 'Q', timestamp: '2026-05-13T00:00:03Z' },
        {
          id: executionId,
          role: 'assistant',
          content: '',
          executionId,
          elapsedMs: 100,
          timestamp: '2026-05-13T00:00:04Z',
        },
      ],
    })
  })
  await waitFor(() =>
    expect(result.current.messages.map((m) => m.id)).toEqual(['q1', 'a1', 'q2', executionId]),
  )

  // The stream has not finished: the next activity frame must still reach the
  // confirmed placeholder instead of being dropped with the old local id.
  await act(async () => {
    stream.enqueue(
      new TextEncoder().encode(
        `data: ${JSON.stringify({ type: 'activity', event: { type: 'working', elapsed_ms: 4200 } })}\n\n`,
      ),
    )
  })
  await waitFor(() => {
    const answer = result.current.messages[result.current.messages.length - 1]
    expect(answer?.elapsedMs).toBe(4200)
    expect(answer?.activityEvents?.map((event) => event.type)).toEqual(['working'])
  })

  const detail = {
    execution: {
      id: executionId,
      conversationId: sessionId,
      requestTurnId: 'q2',
      responseTurnId: 'a2',
      lifecycleState: 'completed',
    },
    requestTurn: { id: 'q2', conversationId: sessionId, turnKind: 'user', contentText: 'Q' },
    responseTurn: {
      id: 'a2',
      conversationId: sessionId,
      turnKind: 'assistant',
      executionId,
      contentText: 'A2',
    },
    runtimeSummary: {
      acceptedAt: '2026-05-13T13:00:00Z',
      completedAt: '2026-05-13T13:00:04Z',
    },
    chunkReferences: [],
    entityReferences: [],
    preparedSegmentReferences: [],
    relationReferences: [],
    technicalFactReferences: [],
    runtimeStageSummaries: [],
    verificationState: 'verified',
    verificationWarnings: [],
  } as unknown as AssistantTurnExecutionResponse
  await act(async () => {
    stream.enqueue(
      new TextEncoder().encode(`data: ${JSON.stringify({ type: 'completed', detail })}\n\n`),
    )
    stream.close()
  })
  await waitFor(() =>
    expect(result.current.messages.map((m) => m.content)).toEqual(['Q', 'A1', 'Q', 'A2']),
  )
  expect(result.current.messages.map((m) => m.id)).toEqual(['q1', 'a1', 'q2', 'a2'])
  expect(fetchMock).toHaveBeenCalledTimes(1)
})

it('keeps confirmed history and shows the failure on the accepted reply after a stream error', async () => {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity } },
  })
  const listKey = queries.listQuerySessionsOptions({ path: { libraryId: 'library-1' } }).queryKey
  const sessionKey = queries.getQuerySessionOptions({ path: { sessionId } }).queryKey
  client.setQueryData(listKey, { items: [session], nextCursor: null, total: 1 })
  client.setQueryData(sessionKey, oldHistory)
  localStorage.setItem(
    'ironrag.v1.assistant.active-session:workspace-1:library-1',
    JSON.stringify(sessionId),
  )
  let stream!: ReadableStreamDefaultController<Uint8Array>
  const fetchMock = vi.fn().mockResolvedValue(
    new Response(
      new ReadableStream({
        start(controller) {
          stream = controller
        },
      }),
      { status: 200, headers: { 'x-ironrag-query-execution-id': executionId } },
    ),
  )
  vi.stubGlobal('fetch', fetchMock)
  const wrapper = ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={client}>{children}</QueryClientProvider>
  )
  const { result } = renderHook(
    () =>
      useAssistantSession({
        workspaceId: 'workspace-1',
        libraryId: 'library-1',
        t: ((key: string) => key) as TFunction,
      }),
    { wrapper },
  )
  await waitFor(() => expect(result.current.messages.map((m) => m.content)).toEqual(['Q', 'A1']))
  act(() => {
    expect(result.current.sendQuestion('Q')).toBe(true)
  })
  await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(1))
  await waitFor(() =>
    expect(result.current.messages.map((m) => m.content)).toEqual(['Q', 'A1', 'Q', '']),
  )

  // The history poll confirms the accepted turn and carries a newer message
  // written in another tab while this stream is still open.
  act(() => {
    client.setQueryData(sessionKey, {
      ...oldHistory,
      messages: [
        ...oldHistory.messages,
        { id: 'q2', role: 'user', content: 'Q', timestamp: '2026-05-13T00:00:03Z' },
        {
          id: executionId,
          role: 'assistant',
          content: '',
          executionId,
          elapsedMs: 100,
          timestamp: '2026-05-13T00:00:04Z',
        },
        { id: 'q-other', role: 'user', content: 'OTHER', timestamp: '2026-05-13T00:00:05Z' },
        {
          id: 'a-other',
          role: 'assistant',
          content: 'OTHER-A',
          executionId: 'other-execution',
          timestamp: '2026-05-13T00:00:06Z',
        },
      ],
    })
  })
  await waitFor(() =>
    expect(result.current.messages.map((m) => m.id)).toEqual([
      'q1',
      'a1',
      'q2',
      executionId,
      'q-other',
      'a-other',
    ]),
  )

  // The server ends the accepted turn with a terminal error.
  await act(async () => {
    stream.enqueue(
      new TextEncoder().encode(
        `data: ${JSON.stringify({ type: 'failed', code: 'internal_error', message: 'boom' })}\n\n`,
      ),
    )
  })
  await waitFor(() =>
    expect(result.current.messages.map((m) => [m.id, m.content])).toEqual([
      ['q1', 'Q'],
      ['a1', 'A1'],
      ['q2', 'Q'],
      [executionId, 'assistant.serverFailure'],
      ['q-other', 'OTHER'],
      ['a-other', 'OTHER-A'],
    ]),
  )
  const failedReply = result.current.messages[3]
  expect(failedReply?.timestamp).toBe('2026-05-13T00:00:04Z')
  expect(failedReply?.elapsedMs).toBe(100)
  expect(failedReply?.executionId).toBe(executionId)
  expect(result.current.activeSession).toBe(sessionId)
  expect(result.current.retryable).toEqual({
    question: 'Q',
    diagnosis: 'assistant.serverFailure',
  })
  expect(fetchMock).toHaveBeenCalledTimes(1)
})
