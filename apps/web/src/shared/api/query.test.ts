import { afterEach, describe, expect, it, vi } from 'vitest'

import { Query } from './generated'
import { queryApi } from './query'
import {
  markSessionSignedIn,
  markSessionSignedOut,
  subscribeToSessionExpired,
} from './sessionLifecycle'

const SESSION_ID = '0192f0aa-0000-7000-8000-000000000001'
const OTHER_SESSION_ID = '0192f0aa-0000-7000-8000-000000000002'
const EXECUTION_ID = '0192f0aa-0000-7000-8000-000000000003'
const OTHER_EXECUTION_ID = '0192f0aa-0000-7000-8000-000000000004'
const REQUEST_TURN_ID = '0192f0aa-0000-7000-8000-000000000005'
const OTHER_TURN_ID = '0192f0aa-0000-7000-8000-000000000006'
const RESPONSE_TURN_ID = '0192f0aa-0000-7000-8000-000000000007'

const QUERY_EXECUTION_HEADER = 'x-ironrag-query-execution-id'

const ACTIVITY_FRAME =
  'event: assistant_turn\n' + 'data: {"type":"activity","event":{"type":"started"}}\n\n'

function streamThatFailsAfterActivity(): ReadableStream<Uint8Array> {
  let pulled = false
  return new ReadableStream<Uint8Array>({
    pull(controller) {
      if (!pulled) {
        pulled = true
        controller.enqueue(new TextEncoder().encode(ACTIVITY_FRAME))
        return
      }
      throw new Error('opaque transport interruption')
    },
  })
}

function streamThatFailsBeforeActivity(): ReadableStream<Uint8Array> {
  return new ReadableStream<Uint8Array>({
    pull() {
      throw new Error('opaque transport interruption')
    },
  })
}

function streamWithBackendFailureEvent(): ReadableStream<Uint8Array> {
  return new ReadableStream<Uint8Array>({
    start(controller) {
      controller.enqueue(
        new TextEncoder().encode(
          ACTIVITY_FRAME +
            'event: assistant_turn\n' +
            'data: {"type":"failed","message":"Error in input stream"}\n\n',
        ),
      )
      controller.close()
    },
  })
}

function turnStreamResponse(
  body: ReadableStream<Uint8Array>,
  executionId: string | null = EXECUTION_ID,
): Response {
  return new Response(body, {
    headers: executionId ? { [QUERY_EXECUTION_HEADER]: executionId } : {},
    status: 200,
  })
}

function executionDetail(params: {
  answer?: string
  conversationId?: string
  executionId: string
  failureCode?: string
  lifecycleState: string
  requestTurnId?: string
  requestTurnObjectId?: string
  responseExecutionId?: string
  responseTurnId?: string
}) {
  const { answer = 'Answer', executionId, failureCode, lifecycleState } = params
  const conversationId = params.conversationId ?? SESSION_ID
  const requestTurnId = params.requestTurnId ?? REQUEST_TURN_ID
  const requestTurnObjectId = params.requestTurnObjectId ?? requestTurnId
  const responseTurnId = params.responseTurnId
  return {
    data: {
      chunkReferences: [],
      contextBundleId: 'bundle-1',
      entityReferences: [],
      execution: {
        contextBundleId: 'bundle-1',
        conversationId,
        failureCode: failureCode ?? null,
        id: executionId,
        libraryId: 'library-1',
        lifecycleState,
        queryText: 'Question',
        requestTurnId,
        responseTurnId: responseTurnId ?? null,
        runtimeExecutionId: 'runtime-1',
        startedAt: '2026-05-13T00:00:00.000Z',
        workspaceId: 'workspace-1',
      },
      preparedSegmentReferences: [],
      relationReferences: [],
      requestTurn: {
        authorPrincipalId: 'principal-1',
        contentText: 'Question',
        conversationId,
        createdAt: '2026-05-13T00:00:00.000Z',
        executionId: null,
        id: requestTurnObjectId,
        turnIndex: 0,
        turnKind: 'user',
      },
      responseTurn: responseTurnId
        ? {
            authorPrincipalId: null,
            contentText: answer,
            conversationId,
            createdAt: '2026-05-13T00:00:01.000Z',
            executionId: params.responseExecutionId ?? executionId,
            id: responseTurnId,
            turnIndex: 1,
            turnKind: 'assistant',
          }
        : null,
      runtimeStageSummaries: [],
      runtimeSummary: {
        acceptedAt: '2026-05-13T00:00:00.000Z',
        lifecycleState,
        parallelActionLimit: 1,
        policySummary: {
          allowCount: 0,
          recentDecisions: [],
          rejectCount: 0,
          terminateCount: 0,
        },
        runtimeExecutionId: 'runtime-1',
        turnBudget: 1,
        turnCount: 1,
      },
      technicalFactReferences: [],
      verificationState: 'verified',
      verificationWarnings: [],
    },
  }
}

function completedExecution(overrides: Partial<Parameters<typeof executionDetail>[0]> = {}) {
  return executionDetail({
    executionId: EXECUTION_ID,
    lifecycleState: 'completed',
    requestTurnId: REQUEST_TURN_ID,
    responseTurnId: RESPONSE_TURN_ID,
    ...overrides,
  })
}

describe('queryApi.createTurnStream', () => {
  afterEach(() => {
    vi.restoreAllMocks()
  })

  it('recovers a completed turn from the execution named by the stream header', async () => {
    vi.spyOn(globalThis, 'fetch').mockResolvedValue(
      turnStreamResponse(streamThatFailsAfterActivity()),
    )
    vi.spyOn(Query, 'getQueryExecution').mockResolvedValue(completedExecution() as never)

    const result = await queryApi.createTurnStream(SESSION_ID, 'Question')

    expect(result.responseTurn?.contentText).toBe('Answer')
    expect(Query.getQueryExecution).toHaveBeenCalledWith({
      path: { executionId: EXECUTION_ID },
      signal: expect.any(AbortSignal),
    })
  })

  it('recovers an accepted turn whose stream died before the first activity frame', async () => {
    const fetchSpy = vi
      .spyOn(globalThis, 'fetch')
      .mockResolvedValue(turnStreamResponse(streamThatFailsBeforeActivity()))
    vi.spyOn(Query, 'getQueryExecution').mockResolvedValue(completedExecution() as never)

    const result = await queryApi.createTurnStream(SESSION_ID, 'Question')

    expect(result.responseTurn?.contentText).toBe('Answer')
    // Recovery is GET-only: the paid turn must never be submitted twice.
    expect(fetchSpy).toHaveBeenCalledTimes(1)
    expect(fetchSpy.mock.calls[0]?.[1]).toMatchObject({ method: 'POST' })
  })

  it('reads the answer from the execution even when another request sits between the turns', async () => {
    const submits = vi
      .spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(turnStreamResponse(streamThatFailsBeforeActivity(), EXECUTION_ID))
      .mockResolvedValueOnce(
        turnStreamResponse(streamThatFailsBeforeActivity(), OTHER_EXECUTION_ID),
      )
    const getSession = vi.spyOn(Query, 'getQuerySession')
    const getExecution = vi
      .spyOn(Query, 'getQueryExecution')
      .mockImplementation(async ({ path }) => {
        if (path.executionId === EXECUTION_ID) return completedExecution() as never
        if (path.executionId !== OTHER_EXECUTION_ID) throw new Error('unknown execution')
        return completedExecution({
          executionId: OTHER_EXECUTION_ID,
          requestTurnId: OTHER_TURN_ID,
          responseTurnId: '0192f0aa-0000-7000-8000-000000000008',
          answer: 'Second answer',
        }) as never
      })
    const [first, second] = await Promise.all([
      queryApi.createTurnStream(SESSION_ID, 'Question'),
      queryApi.createTurnStream(SESSION_ID, 'Question'),
    ])
    expect(first.responseTurn?.id).toBe(RESPONSE_TURN_ID)
    expect(second.requestTurn?.id).toBe(OTHER_TURN_ID)
    expect(second.responseTurn?.contentText).toBe('Second answer')
    expect(submits).toHaveBeenCalledTimes(2)
    expect(getSession).not.toHaveBeenCalled()
    expect(getExecution).toHaveBeenCalledTimes(2)
  })

  it('waits out a running execution instead of rendering it as the answer', async () => {
    vi.spyOn(globalThis, 'fetch').mockResolvedValue(
      turnStreamResponse(streamThatFailsBeforeActivity()),
    )
    const getExecution = vi
      .spyOn(Query, 'getQueryExecution')
      .mockResolvedValueOnce(
        executionDetail({
          executionId: EXECUTION_ID,
          lifecycleState: 'running',
          requestTurnId: REQUEST_TURN_ID,
        }) as never,
      )
      .mockResolvedValueOnce(completedExecution() as never)

    const result = await queryApi.createTurnStream(SESSION_ID, 'Question')

    expect(result.responseTurn?.contentText).toBe('Answer')
    expect(getExecution).toHaveBeenCalledTimes(2)
  })

  it('waits a bounded time for an execution that is not seeded yet', async () => {
    vi.spyOn(globalThis, 'fetch').mockResolvedValue(
      turnStreamResponse(streamThatFailsBeforeActivity()),
    )
    const getExecution = vi
      .spyOn(Query, 'getQueryExecution')
      .mockResolvedValueOnce({
        error: { detail: 'not found' },
        response: new Response(null, { status: 404 }),
      } as never)
      .mockResolvedValueOnce(completedExecution() as never)

    const result = await queryApi.createTurnStream(SESSION_ID, 'Question')

    expect(result.responseTurn?.contentText).toBe('Answer')
    expect(getExecution).toHaveBeenCalledTimes(2)
  })

  it('does not recover an execution from another session', async () => {
    vi.spyOn(globalThis, 'fetch').mockResolvedValue(
      turnStreamResponse(streamThatFailsBeforeActivity()),
    )
    const getExecution = vi
      .spyOn(Query, 'getQueryExecution')
      .mockResolvedValue(completedExecution({ conversationId: OTHER_SESSION_ID }) as never)

    await expect(queryApi.createTurnStream(SESSION_ID, 'Question')).rejects.toThrow(
      'opaque transport interruption',
    )
    expect(getExecution).toHaveBeenCalledTimes(1)
  })

  it('does not recover when the server echoes a different execution identifier', async () => {
    vi.spyOn(globalThis, 'fetch').mockResolvedValue(
      turnStreamResponse(streamThatFailsBeforeActivity()),
    )
    vi.spyOn(Query, 'getQueryExecution').mockResolvedValue(
      completedExecution({ executionId: OTHER_EXECUTION_ID }) as never,
    )

    await expect(queryApi.createTurnStream(SESSION_ID, 'Question')).rejects.toThrow(
      'opaque transport interruption',
    )
  })

  it('does not recover when the request turn is not the linked one', async () => {
    vi.spyOn(globalThis, 'fetch').mockResolvedValue(
      turnStreamResponse(streamThatFailsBeforeActivity()),
    )
    vi.spyOn(Query, 'getQueryExecution').mockResolvedValue(
      completedExecution({ requestTurnObjectId: OTHER_TURN_ID }) as never,
    )

    await expect(queryApi.createTurnStream(SESSION_ID, 'Question')).rejects.toThrow(
      'opaque transport interruption',
    )
  })

  it('does not recover when the response turn belongs to another execution', async () => {
    vi.spyOn(globalThis, 'fetch').mockResolvedValue(
      turnStreamResponse(streamThatFailsBeforeActivity()),
    )
    vi.spyOn(Query, 'getQueryExecution').mockResolvedValue(
      completedExecution({ responseExecutionId: OTHER_EXECUTION_ID }) as never,
    )

    await expect(queryApi.createTurnStream(SESSION_ID, 'Question')).rejects.toThrow(
      'opaque transport interruption',
    )
  })

  it('does not present a durable turn that ended without an answer as recovered', async () => {
    vi.spyOn(globalThis, 'fetch').mockResolvedValue(
      turnStreamResponse(streamThatFailsBeforeActivity()),
    )
    const getExecution = vi.spyOn(Query, 'getQueryExecution').mockResolvedValue(
      executionDetail({
        executionId: EXECUTION_ID,
        failureCode: 'knowledge_not_ready',
        lifecycleState: 'failed',
        requestTurnId: REQUEST_TURN_ID,
      }) as never,
    )

    await expect(queryApi.createTurnStream(SESSION_ID, 'Question')).rejects.toThrow(
      'opaque transport interruption',
    )
    // A terminal failure is final: no polling until the recovery budget ends.
    expect(getExecution).toHaveBeenCalledTimes(1)
  })

  it('does not render a non-assistant or empty response turn', async () => {
    vi.spyOn(globalThis, 'fetch').mockImplementation(async () =>
      turnStreamResponse(streamThatFailsBeforeActivity()),
    )
    const wrongRole = completedExecution()
    wrongRole.data.responseTurn!.turnKind = 'user'
    vi.spyOn(Query, 'getQueryExecution')
      .mockResolvedValueOnce(wrongRole as never)
      .mockResolvedValueOnce(completedExecution({ answer: ' ' }) as never)
    for (let i = 0; i < 2; i++) {
      await expect(queryApi.createTurnStream(SESSION_ID, 'Question')).rejects.toThrow(
        'opaque transport interruption',
      )
    }
  })

  it('surfaces a forbidden recovery read instead of hiding it', async () => {
    vi.spyOn(globalThis, 'fetch').mockResolvedValue(
      turnStreamResponse(streamThatFailsBeforeActivity()),
    )
    vi.spyOn(Query, 'getQueryExecution').mockResolvedValue({
      error: { detail: 'forbidden' },
      response: new Response(null, { status: 403 }),
    } as never)

    await expect(queryApi.createTurnStream(SESSION_ID, 'Question')).rejects.toMatchObject({
      status: 403,
    })
  })

  it('surfaces a revoked session from the recovery read instead of a stale answer', async () => {
    vi.spyOn(globalThis, 'fetch').mockResolvedValue(
      turnStreamResponse(streamThatFailsBeforeActivity()),
    )
    vi.spyOn(Query, 'getQueryExecution').mockResolvedValue({
      error: { detail: 'unauthorized' },
      response: new Response(null, { status: 401 }),
    } as never)

    await expect(queryApi.createTurnStream(SESSION_ID, 'Question')).rejects.toMatchObject({
      status: 401,
    })
  })

  it('does not fall back to anything when the stream carries no execution handle', async () => {
    vi.spyOn(globalThis, 'fetch').mockResolvedValue(
      turnStreamResponse(streamThatFailsBeforeActivity(), null),
    )
    const getExecution = vi.spyOn(Query, 'getQueryExecution')

    await expect(queryApi.createTurnStream(SESSION_ID, 'Question')).rejects.toThrow(
      'opaque transport interruption',
    )
    expect(getExecution).not.toHaveBeenCalled()
  })

  it('keeps a definite pre-stream HTTP failure instead of recovering', async () => {
    vi.spyOn(globalThis, 'fetch').mockResolvedValue(
      new Response(JSON.stringify({ detail: 'upstream unavailable' }), {
        headers: { 'Content-Type': 'application/json' },
        status: 502,
      }),
    )
    const getExecution = vi.spyOn(Query, 'getQueryExecution')

    await expect(queryApi.createTurnStream(SESSION_ID, 'Question')).rejects.toMatchObject({
      status: 502,
    })
    expect(getExecution).not.toHaveBeenCalled()
  })

  it('ends the assistant stream through the session lifecycle when the session died', async () => {
    // The SSE turn is a hand-rolled fetch, so it does not pass through the
    // generated client's interceptor. It must still report the dead session,
    // and it must not fall into durable-session recovery — that would poll a
    // door the server has already closed.
    vi.spyOn(globalThis, 'fetch').mockResolvedValue(
      new Response(JSON.stringify({ detail: 'unauthorized' }), {
        status: 401,
        headers: { 'Content-Type': 'application/json' },
      }),
    )
    const getExecution = vi.spyOn(Query, 'getQueryExecution')
    const expired = vi.fn()
    const unsubscribe = subscribeToSessionExpired(expired)
    markSessionSignedIn()

    try {
      await expect(queryApi.createTurnStream(SESSION_ID, 'Question')).rejects.toMatchObject({
        status: 401,
      })
      expect(expired).toHaveBeenCalledTimes(1)
      expect(getExecution).not.toHaveBeenCalled()
    } finally {
      unsubscribe()
      markSessionSignedOut()
    }
  })

  it('does not hide backend failure events behind durable-session recovery', async () => {
    vi.spyOn(globalThis, 'fetch').mockResolvedValue(
      turnStreamResponse(streamWithBackendFailureEvent()),
    )
    const getExecution = vi.spyOn(Query, 'getQueryExecution')

    await expect(queryApi.createTurnStream(SESSION_ID, 'Question')).rejects.toThrow(
      'Error in input stream',
    )
    expect(getExecution).not.toHaveBeenCalled()
  })
})

describe('queryApi session mutations', () => {
  afterEach(() => {
    vi.restoreAllMocks()
  })

  it('renames a session through the generated PATCH operation', async () => {
    const renamed = {
      conversation_state: 'active',
      created_at: '2026-05-13T00:00:00.000Z',
      created_by_principal_id: 'principal-1',
      id: 'session-1',
      library_id: 'library-1',
      title: 'Durable title',
      updated_at: '2026-05-13T00:00:01.000Z',
      workspace_id: 'workspace-1',
    }
    const rename = vi
      .spyOn(Query, 'renameQuerySession')
      .mockResolvedValue({ data: renamed } as never)

    await expect(queryApi.renameSession('session-1', 'Durable title')).resolves.toEqual(renamed)
    expect(rename).toHaveBeenCalledWith({
      body: { title: 'Durable title' },
      path: { sessionId: 'session-1' },
    })
  })

  it('deletes a session through the generated DELETE operation', async () => {
    const remove = vi
      .spyOn(Query, 'deleteQuerySession')
      .mockResolvedValue({ data: undefined } as never)

    await expect(queryApi.deleteSession('session-1')).resolves.toBeUndefined()
    expect(remove).toHaveBeenCalledWith({ path: { sessionId: 'session-1' } })
  })
})
