import type { SetStateAction } from 'react';
import type { AssistantMessage, EvidenceBundle } from '@/shared/types';
import type { AssistantTurnExecutionResponse } from '@/shared/api/query';
import { mapAssistantTurnToEvidence } from '@/features/assistant/model/assistantAdapter';

export const STARTER_PROMPT_IDS = [
  'overview',
  'keyPoints',
  'openQuestions',
  'recentItems',
] as const;

export const EMPTY_MESSAGES: AssistantMessage[] = [];

export type RetryableAssistantTurn = {
  question: string;
  diagnosis: string;
};

export function resolveStateAction<T>(action: SetStateAction<T>, previous: T): T {
  return typeof action === 'function'
    ? (action as (previousValue: T) => T)(previous)
    : action;
}

export function isTransientNetworkReject(message: string): boolean {
  const lower = message.toLowerCase();
  return (
    lower.includes('networkerror') ||
    lower.includes('input stream') ||
    lower.includes('failed to fetch') ||
    lower.includes('load failed') ||
    lower.includes('body stream') ||
    lower.includes('timeout') ||
    lower.includes('abort')
  );
}

const TURN_RETRY_MAX_ATTEMPTS = 3;
const TURN_RETRY_BASE_DELAY_MS = 1000;
const TURN_RETRY_BACKOFF_FACTOR = 3;

/** Calls `createTurn` with exponential-backoff retry for transient
 *  network errors (timeout, connection reset, etc.).  Non-transient
 *  errors (4xx, 5xx) are re-thrown immediately. */
export async function createTurnWithRetry(
  sessionId: string,
  questionText: string,
  createTurn: (sessionId: string, contentText: string) => Promise<AssistantTurnExecutionResponse>,
): Promise<AssistantTurnExecutionResponse> {
  for (let attempt = 0; attempt <= TURN_RETRY_MAX_ATTEMPTS; attempt++) {
    try {
      return await createTurn(sessionId, questionText);
    } catch (err: unknown) {
      const msg = typeof err === 'object' && err !== null && 'message' in err
        ? String(err.message)
        : String(err);
      if (!isTransientNetworkReject(msg) || attempt >= TURN_RETRY_MAX_ATTEMPTS) {
        throw err;
      }
      // Exponential backoff: 1s, 3s, 9s
      const delay = TURN_RETRY_BASE_DELAY_MS * TURN_RETRY_BACKOFF_FACTOR ** attempt;
      await new Promise((resolve) => setTimeout(resolve, delay));
    }
  }
  throw new Error('unreachable');
}

export function createUserMessage(question: string, now: number): AssistantMessage {
  return {
    id: `m-${now}`,
    role: 'user',
    content: question,
    timestamp: new Date(now).toISOString(),
  };
}

export function createPendingAssistantMessage(now: number): AssistantMessage {
  return {
    id: `m-pending-${now}`,
    role: 'assistant',
    content: '',
    timestamp: new Date(now).toISOString(),
  };
}

export function createErrorAssistantMessage(content: string): AssistantMessage {
  return {
    id: `m-err-${Date.now()}`,
    role: 'assistant',
    content,
    timestamp: new Date().toISOString(),
  };
}

/**
 * Server-authoritative turn wall-clock in milliseconds, measured entirely on
 * the API host so it is immune to client↔server clock skew. Prefers the
 * execution window (`completedAt − startedAt`); falls back to the request →
 * response turn timestamps, which are likewise both server-stamped. Returns
 * `undefined` when no consistent server pair is available.
 */
export function serverTurnDurationMs(
  result: AssistantTurnExecutionResponse,
): number | undefined {
  const serverPairs: Array<[string | null | undefined, string | null | undefined]> = [
    [result.execution?.startedAt, result.execution?.completedAt],
    [result.requestTurn?.createdAt, result.responseTurn?.createdAt],
  ];
  for (const [startIso, endIso] of serverPairs) {
    if (!startIso || !endIso) continue;
    const start = Date.parse(startIso);
    const end = Date.parse(endIso);
    if (Number.isFinite(start) && Number.isFinite(end) && end >= start) {
      return end - start;
    }
  }
  return undefined;
}

/**
 * Replaces the optimistic pending assistant message with the finalized turn.
 *
 * The optimistic user message is stamped with the *browser* clock at send time,
 * whereas the turn's authoritative timestamps come from the API host. Computing
 * "Reply: N s" as `serverAnswerTimestamp − browserUserTimestamp` therefore folds
 * any client↔server clock skew straight into the displayed latency (a lagging
 * browser clock inflates it). We instead carry the single-clock server duration
 * as `durationMs`, and restamp both the request and response messages from their
 * server turn timestamps so the two visible times stay mutually consistent and
 * match what a later history reload renders.
 */
export function applyTurnResultToMessages(
  messages: AssistantMessage[],
  pendingId: string,
  result: AssistantTurnExecutionResponse,
  emptyAnswerText: string,
): AssistantMessage[] {
  const answerText = result.responseTurn?.contentText ?? emptyAnswerText;
  const evidence = mapAssistantTurnToEvidence(result);
  const durationMs = serverTurnDurationMs(result);
  const answerTimestamp = result.execution?.completedAt ?? result.responseTurn?.createdAt;
  // Always a server timestamp: prefer the user turn's own stamp, else the
  // execution start. This guarantees the restamped question shares a clock with
  // the server-stamped answer even in degenerate responses, so the reload-style
  // timestamp-delta fallback can never silently subtract a browser timestamp
  // from a server one.
  const questionTimestamp = result.requestTurn?.createdAt ?? result.execution?.startedAt;

  // The user turn that triggered this answer is the nearest message preceding
  // the pending placeholder; restamp it from the server so the question and
  // answer share one clock.
  const executionId = result.execution?.id;
  const pendingIndex = messages.findIndex(
    (message) =>
      message.id === pendingId ||
      (executionId &&
        message.role === 'assistant' &&
        !message.content &&
        message.executionId === executionId),
  );
  let userIndex = -1;
  for (let i = pendingIndex - 1; i >= 0; i -= 1) {
    if (messages[i]?.role === 'user') {
      userIndex = i;
      break;
    }
  }

  return messages.map((message, index) => {
    if (index === pendingIndex) {
      return {
        id: result.responseTurn?.id ?? pendingId,
        role: 'assistant',
        content: answerText,
        timestamp: answerTimestamp ?? message.timestamp,
        ...(durationMs !== undefined ? { durationMs } : {}),
        executionId: result.responseTurn?.executionId ?? null,
        evidence,
      };
    }
    if (index === userIndex && questionTimestamp) {
      return { ...message, timestamp: questionTimestamp };
    }
    return message;
  });
}

/**
 * Total distinct evidence sources backing a message — used to drive the
 * "See all N sources" affordance when the inline chip list is capped. Counts
 * distinct source documents from segment refs plus entity references.
 */
export function countDistinctSources(message: AssistantMessage): number {
  const evidence = message.evidence;
  if (!evidence) return 0;
  const documents = new Set<string>();
  for (const ref of evidence.segmentRefs) {
    documents.add(ref.documentId || ref.documentName);
  }
  return documents.size + evidence.entityRefs.length;
}

export function latestEvidenceFromMessages(
  messages: AssistantMessage[],
): EvidenceBundle | undefined {
  for (let i = messages.length - 1; i >= 0; i -= 1) {
    const message = messages[i];
    if (message?.role === 'assistant' && message.evidence) {
      return message.evidence;
    }
  }
  return undefined;
}
