import { describe, expect, it } from 'vitest'

import type { AssistantTurnExecutionResponse } from '@/shared/api/query'

import {
  applyTurnResultToMessages,
  ASSISTANT_IN_PROGRESS_LIMIT_MS,
  createPendingAssistantMessage,
  createUserMessage,
  isInProgressAssistantTurn,
  sessionMessagesHydrationKey,
  serverTurnDurationMs,
} from './assistantPageState'

function turnResult(): AssistantTurnExecutionResponse {
  return {
    chunkReferences: [],
    entityReferences: [],
    preparedSegmentReferences: [],
    relationReferences: [],
    technicalFactReferences: [],
    runtimeStageSummaries: [],
    verificationState: 'verified',
    verificationWarnings: [],
    responseTurn: {
      contentText: 'Durable answer',
      conversationId: 'session-1',
      createdAt: '2026-08-09T10:00:04Z',
      executionId: 'execution-1',
      id: 'answer-1',
      turnIndex: 2,
      turnKind: 'assistant',
    },
    runtimeSummary: {
      acceptedAt: '2026-08-09T10:00:01Z',
      completedAt: '2026-08-09T10:00:04Z',
    },
  } as unknown as AssistantTurnExecutionResponse
}

describe('assistant page turn state', () => {
  it('uses the server runtime clock for the displayed answer duration', () => {
    const result = turnResult()
    expect(serverTurnDurationMs(result)).toBe(3_000)

    const user = createUserMessage('Question', Date.parse('2026-08-09T09:58:00Z'))
    const pending = createPendingAssistantMessage(Date.parse('2026-08-09T09:58:00Z'))
    const messages = applyTurnResultToMessages([user, pending], pending.id, result, 'No answer')

    expect(messages[0]?.timestamp).toBe('2026-08-09T10:00:01Z')
    expect(messages[1]?.timestamp).toBe('2026-08-09T10:00:04Z')
    expect(messages[1]?.durationMs).toBe(3_000)
  })
})

describe('isInProgressAssistantTurn', () => {
  it('treats the optimistic local placeholder as in progress', () => {
    expect(isInProgressAssistantTurn(createPendingAssistantMessage(1))).toBe(true)
  })

  it('treats a server pending placeholder with elapsed time as in progress', () => {
    expect(
      isInProgressAssistantTurn({
        id: 'execution-1',
        role: 'assistant',
        content: '',
        timestamp: '2026-08-19T00:00:00Z',
        elapsedMs: 12_345,
        executionId: 'execution-1',
      }),
    ).toBe(true)
  })

  it('does not treat a finished empty assistant turn as in progress', () => {
    expect(
      isInProgressAssistantTurn({
        id: 'answer-1',
        role: 'assistant',
        content: '',
        timestamp: '2026-08-19T00:00:00Z',
        executionId: 'execution-1',
      }),
    ).toBe(false)
  })

  it('does not treat a stale pending placeholder as still running', () => {
    expect(
      isInProgressAssistantTurn({
        id: 'execution-1',
        role: 'assistant',
        content: '',
        timestamp: '2026-08-19T00:00:00Z',
        elapsedMs: ASSISTANT_IN_PROGRESS_LIMIT_MS,
        executionId: 'execution-1',
      }),
    ).toBe(false)
  })
})

describe('sessionMessagesHydrationKey', () => {
  it('ignores ticking elapsedMs so a pending poll does not look like a new conversation', () => {
    const first = {
      id: 'execution-1',
      role: 'assistant' as const,
      content: '',
      timestamp: '2026-08-19T00:00:00Z',
      elapsedMs: 1_000,
      executionId: 'execution-1',
    }
    const later = { ...first, elapsedMs: 2_000 }
    expect(sessionMessagesHydrationKey('session-1', [first])).toBe(
      sessionMessagesHydrationKey('session-1', [later]),
    )
  })

  it('changes when a pending placeholder goes stale', () => {
    const pending = {
      id: 'execution-1',
      role: 'assistant' as const,
      content: '',
      timestamp: '2026-08-19T00:00:00Z',
      elapsedMs: 1_000,
      executionId: 'execution-1',
    }
    const stale = { ...pending, elapsedMs: ASSISTANT_IN_PROGRESS_LIMIT_MS }
    expect(sessionMessagesHydrationKey('session-1', [pending])).not.toBe(
      sessionMessagesHydrationKey('session-1', [stale]),
    )
  })
})
