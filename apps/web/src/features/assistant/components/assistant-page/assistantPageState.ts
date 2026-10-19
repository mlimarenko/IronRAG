import type { SetStateAction } from 'react'
import type { AssistantMessage, EvidenceBundle } from '@/shared/types'
import type { AssistantTurnExecutionResponse } from '@/shared/api/query'
import { mapAssistantTurnToEvidence } from '@/features/assistant/model/assistantAdapter'
import type { AssistantEvidenceView } from '@/features/assistant/model/assistantAdapter'

export const EMPTY_MESSAGES: AssistantMessage[] = []

export type RetryableAssistantTurn = {
  question: string
  diagnosis: string
}

export function resolveStateAction<T>(action: SetStateAction<T>, previous: T): T {
  return typeof action === 'function' ? (action as (previousValue: T) => T)(previous) : action
}

export function createUserMessage(question: string, now: number): AssistantMessage {
  return {
    id: `m-${now}`,
    role: 'user',
    content: question,
    timestamp: new Date(now).toISOString(),
  }
}

export function createPendingAssistantMessage(now: number): AssistantMessage {
  return {
    id: `m-pending-${now}`,
    role: 'assistant',
    content: '',
    timestamp: new Date(now).toISOString(),
  }
}

/** Matches the browser turn-stream budget in `query.ts` (`TURN_TIMEOUT_MS`). */
export const ASSISTANT_IN_PROGRESS_LIMIT_MS = 195_000

/**
 * Empty assistant content is in-progress only for the optimistic placeholder
 * or a server pending execution that still reports elapsed time. A finished
 * turn with an empty body is an outcome, not a spinner.
 */
export function isInProgressAssistantTurn(message: AssistantMessage): boolean {
  if (message.role !== 'assistant') return false
  if (message.content.trim().length > 0) return false
  if (typeof message.elapsedMs === 'number') {
    return message.elapsedMs < ASSISTANT_IN_PROGRESS_LIMIT_MS
  }
  return message.executionId == null
}

export function sessionMessagesHydrationKey(
  sessionId: string,
  messages: readonly AssistantMessage[],
): string {
  return [
    sessionId,
    messages
      .map((message) =>
        [
          message.id,
          message.role,
          message.executionId ?? '',
          message.timestamp,
          message.content,
          isInProgressAssistantTurn(message) ? '1' : '0',
        ].join('\u001f'),
      )
      .join('\u001e'),
  ].join('\u001d')
}

/**
 * Active runtime wall-clock in milliseconds, measured entirely on the API
 * host. Lifecycle duration is never inferred from query or turn timestamps.
 */
export function serverTurnDurationMs(result: AssistantTurnExecutionResponse): number | undefined {
  const startIso = result.runtimeSummary?.acceptedAt
  const endIso = result.runtimeSummary?.completedAt
  if (!startIso || !endIso) return undefined

  const start = Date.parse(startIso)
  const end = Date.parse(endIso)
  return Number.isFinite(start) && Number.isFinite(end) && end >= start ? end - start : undefined
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
  const answerText = result.responseTurn?.contentText ?? emptyAnswerText
  const evidence = mapAssistantTurnToEvidence(result)
  const durationMs = serverTurnDurationMs(result)
  const answerTimestamp = result.runtimeSummary?.completedAt ?? result.responseTurn?.createdAt
  const questionTimestamp = result.runtimeSummary?.acceptedAt

  // The user turn that triggered this answer is the nearest message preceding
  // the pending placeholder; restamp it from the server so the question and
  // answer share one clock.
  const executionId = result.execution?.id
  const pendingIndex = messages.findIndex(
    (message) =>
      message.id === pendingId ||
      (executionId &&
        message.role === 'assistant' &&
        !message.content &&
        message.executionId === executionId),
  )
  let userIndex = -1
  for (let i = pendingIndex - 1; i >= 0; i -= 1) {
    if (messages[i]?.role === 'user') {
      userIndex = i
      break
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
      }
    }
    if (index === userIndex && questionTimestamp) {
      return { ...message, timestamp: questionTimestamp }
    }
    return message
  })
}

/**
 * Total distinct evidence sources backing a message — used to drive the
 * "See all N sources" affordance when the inline chip list is capped. Counts
 * distinct source documents from segment refs plus entity references.
 */
export function countDistinctSources(message: AssistantMessage): number {
  const evidence = message.evidence
  if (!evidence) return 0
  const evidenceView = evidence as AssistantEvidenceView
  if (evidenceView.referenceSummary) {
    return evidenceView.referenceSummary.totalCount
  }
  const documents = new Set<string>()
  for (const ref of evidence.segmentRefs) {
    documents.add(ref.documentId || ref.documentName)
  }
  return documents.size + evidence.entityRefs.length + (evidenceView.chunkRefs?.length ?? 0)
}

export function latestEvidenceFromMessages(
  messages: AssistantMessage[],
): EvidenceBundle | undefined {
  for (let i = messages.length - 1; i >= 0; i -= 1) {
    const message = messages[i]
    if (message?.role === 'assistant' && message.evidence) {
      return message.evidence
    }
  }
  return undefined
}
