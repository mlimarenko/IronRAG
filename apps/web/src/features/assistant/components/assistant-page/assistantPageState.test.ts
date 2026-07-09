import { describe, expect, it } from 'vitest';
import type { AssistantMessage } from '@/shared/types';
import type { AssistantTurnExecutionResponse } from '@/shared/api/query';
import {
  applyTurnResultToMessages,
  createPendingAssistantMessage,
  createUserMessage,
  serverTurnDurationMs,
} from './assistantPageState';

type TurnResultOverrides = {
  startedAt?: string | null;
  completedAt?: string | null;
  requestCreatedAt?: string | null;
  responseCreatedAt?: string | null;
  answerText?: string;
  withResponseTurn?: boolean;
};

/**
 * Minimal `AssistantExecutionDetail` shaped to exercise the fields
 * `applyTurnResultToMessages` / `serverTurnDurationMs` actually read.
 */
function buildTurnResult(overrides: TurnResultOverrides = {}): AssistantTurnExecutionResponse {
  const {
    startedAt = '2026-06-06T21:41:33.000Z',
    completedAt = '2026-06-06T21:41:53.000Z',
    requestCreatedAt = '2026-06-06T21:41:33.000Z',
    responseCreatedAt = '2026-06-06T21:41:53.000Z',
    answerText = 'Answer body',
    withResponseTurn = true,
  } = overrides;

  return {
    chunkReferences: [],
    contextBundleId: 'bundle-1',
    entityReferences: [],
    execution: {
      contextBundleId: 'bundle-1',
      conversationId: 'session-1',
      id: 'execution-1',
      libraryId: 'library-1',
      lifecycleState: 'succeeded',
      queryText: 'Question',
      startedAt: startedAt as string,
      completedAt,
      workspaceId: 'workspace-1',
    },
    preparedSegmentReferences: [],
    relationReferences: [],
    requestTurn: requestCreatedAt
      ? {
          authorPrincipalId: null,
          contentText: 'Question',
          conversationId: 'session-1',
          createdAt: requestCreatedAt,
          executionId: 'execution-1',
          id: 'turn-user',
          turnIndex: 1,
          turnKind: 'user',
        }
      : null,
    responseTurn: withResponseTurn
      ? {
          authorPrincipalId: null,
          contentText: answerText,
          conversationId: 'session-1',
          createdAt: responseCreatedAt as string,
          executionId: 'execution-1',
          id: 'turn-assistant',
          turnIndex: 2,
          turnKind: 'assistant',
        }
      : null,
    runtimeStageSummaries: [],
    runtimeSummary: {
      acceptedAt: startedAt as string,
      lifecycleState: 'succeeded',
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
  } as unknown as AssistantTurnExecutionResponse;
}

describe('serverTurnDurationMs', () => {
  it('measures the execution window on the single server clock', () => {
    // 20 s server window — note the absolute timestamps are ~73 s ahead of a
    // browser clock, but that offset must not leak into the duration.
    expect(serverTurnDurationMs(buildTurnResult())).toBe(20_000);
  });

  it('falls back to request → response turn timestamps when completedAt is null', () => {
    const result = buildTurnResult({
      completedAt: null,
      requestCreatedAt: '2026-06-06T21:41:33.000Z',
      responseCreatedAt: '2026-06-06T21:41:45.500Z',
    });
    expect(serverTurnDurationMs(result)).toBe(12_500);
  });

  it('returns undefined when no consistent server pair is available', () => {
    const result = buildTurnResult({
      completedAt: null,
      requestCreatedAt: null,
      withResponseTurn: false,
    });
    expect(serverTurnDurationMs(result)).toBeUndefined();
  });
});

describe('applyTurnResultToMessages', () => {
  // The user message is stamped on the browser clock (here 73 s behind the
  // server), the canonical clock-skew scenario that previously inflated the
  // displayed latency to ~93 s.
  const userBrowserTs = '2026-06-06T21:40:20.000Z';

  function seedMessages(): { messages: AssistantMessage[]; pendingId: string } {
    const sendMs = Date.parse(userBrowserTs);
    const user = { ...createUserMessage('Question', sendMs) };
    const pending = createPendingAssistantMessage(sendMs);
    return { messages: [user, pending], pendingId: pending.id };
  }

  it('uses the server-authoritative duration, not the cross-clock delta', () => {
    const { messages, pendingId } = seedMessages();
    const result = buildTurnResult();

    const next = applyTurnResultToMessages(messages, pendingId, result, 'No answer');
    const answer = next.find((m) => m.role === 'assistant');

    // 20 s server window, NOT completedAt − browser-userTimestamp (~93 s).
    expect(answer?.durationMs).toBe(20_000);
    const crossClockDelta = Date.parse('2026-06-06T21:41:53.000Z') - Date.parse(userBrowserTs);
    expect(answer?.durationMs).not.toBe(crossClockDelta);
  });

  it('restamps both visible timestamps from the server so they share one clock', () => {
    const { messages, pendingId } = seedMessages();
    const result = buildTurnResult();

    const next = applyTurnResultToMessages(messages, pendingId, result, 'No answer');
    const user = next.find((m) => m.role === 'user');
    const answer = next.find((m) => m.role === 'assistant');

    expect(user?.timestamp).toBe('2026-06-06T21:41:33.000Z'); // requestTurn.createdAt
    expect(answer?.timestamp).toBe('2026-06-06T21:41:53.000Z'); // execution.completedAt
    // Visible gap now equals the reported duration.
    const gap = Date.parse(answer!.timestamp) - Date.parse(user!.timestamp);
    expect(gap).toBe(answer?.durationMs);
  });

  it('keeps both timestamps server-stamped even with no requestTurn and no duration', () => {
    // Degenerate response: no requestTurn, completedAt null → durationMs is
    // undefined, so the UI uses the reload-style timestamp-delta fallback. The
    // user message must NOT be left on the browser clock, or that delta would
    // re-introduce the skew (serverAnswerTs − browserUserTs).
    const { messages, pendingId } = seedMessages();
    const result = buildTurnResult({
      completedAt: null,
      requestCreatedAt: null,
      responseCreatedAt: '2026-06-06T21:41:53.000Z',
    });

    const next = applyTurnResultToMessages(messages, pendingId, result, 'No answer');
    const user = next.find((m) => m.role === 'user');
    const answer = next.find((m) => m.role === 'assistant');

    expect(answer?.durationMs).toBeUndefined();
    // User restamped to the server execution start, not left at the browser ts.
    expect(user?.timestamp).toBe('2026-06-06T21:41:33.000Z'); // execution.startedAt
    expect(user?.timestamp).not.toBe(userBrowserTs);
    expect(answer?.timestamp).toBe('2026-06-06T21:41:53.000Z'); // responseTurn.createdAt
    // Fallback delta is now a single-clock (server) measurement.
    const fallbackDelta = Date.parse(answer!.timestamp) - Date.parse(user!.timestamp);
    expect(fallbackDelta).toBe(20_000);
  });

  it('replaces the pending answer content and execution metadata', () => {
    const { messages, pendingId } = seedMessages();
    const result = buildTurnResult({ answerText: 'Grounded reply' });

    const next = applyTurnResultToMessages(messages, pendingId, result, 'No answer');
    const answer = next.find((m) => m.role === 'assistant');

    expect(answer?.content).toBe('Grounded reply');
    expect(answer?.id).toBe('turn-assistant');
    expect(answer?.executionId).toBe('execution-1');
  });

  it('replaces a server-hydrated pending execution when the local pending id is gone', () => {
    const sendMs = Date.parse(userBrowserTs);
    const user = { ...createUserMessage('Question', sendMs), id: 'turn-user' };
    const serverPending: AssistantMessage = {
      id: 'execution-1',
      role: 'assistant',
      content: '',
      timestamp: '2026-06-06T21:41:33.000Z',
      executionId: 'execution-1',
    };
    const result = buildTurnResult({ answerText: 'Hydrated pending resolved' });

    const next = applyTurnResultToMessages(
      [user, serverPending],
      'm-pending-lost',
      result,
      'No answer',
    );
    const answer = next.find((m) => m.role === 'assistant');

    expect(answer?.content).toBe('Hydrated pending resolved');
    expect(answer?.id).toBe('turn-assistant');
    expect(answer?.executionId).toBe('execution-1');
    expect(next).toHaveLength(2);
  });
});
