import { Query } from './generated'
import { ApiError, unwrap } from './runtime'
import { reportUnauthorizedResponse } from './sessionLifecycle'
import type {
  AssistantExecutionDetail,
  AssistantHydratedConversation,
  AssistantSessionListItem,
  AssistantSystemPromptResponse,
  LlmContextSnapshot,
} from './generated'
import type { AssistantAgentActivityEvent } from '@/shared/types'

export type AssistantTurnExecutionResponse = AssistantExecutionDetail
export type LlmContextDebugResponse = LlmContextSnapshot

/** Backend agent turns are capped at 180s; browser budgets leave shutdown headroom. */
const TURN_TIMEOUT_MS = 195_000
const STREAM_RECOVERY_TIMEOUT_MS = 195_000
const STREAM_RECOVERY_INTERVAL_MS = 1_000

type AssistantTurnStreamEvent =
  | { type: 'activity'; event: AssistantAgentActivityEvent }
  | { type: 'completed'; detail: AssistantTurnExecutionResponse }
  | { type: 'failed'; code?: string; message: string }

type AssistantTurnActivityHandler = (event: AssistantAgentActivityEvent) => void

/**
 * A turn the server ended deliberately, with the same stable code its problem
 * responses carry. Without the code the screen had only prose to work with and
 * fell back to a generic apology telling the reader to try again — wrong advice
 * for a library that is still indexing, and for most other reasons a turn ends.
 */
export class AssistantTurnFailedEventError extends Error {
  readonly code: string | null

  constructor(message: string, code?: string) {
    super(message)
    this.name = 'AssistantTurnFailedEventError'
    this.code = code ?? null
  }
}

function parseSseBlock(block: string): AssistantTurnStreamEvent | null {
  const data = block
    .split(/\r?\n/)
    .filter((line) => line.startsWith('data:'))
    .map((line) => line.slice(5).trimStart())
    .join('\n')
    .trim()
  if (!data) return null
  return JSON.parse(data) as AssistantTurnStreamEvent
}

function handleStreamEvent(
  event: AssistantTurnStreamEvent | null,
  onActivity?: AssistantTurnActivityHandler,
): AssistantTurnExecutionResponse | null {
  if (!event) return null
  if (event.type === 'activity') {
    onActivity?.(event.event)
    return null
  }
  if (event.type === 'completed') return event.detail
  throw new AssistantTurnFailedEventError(event.message, event.code)
}

async function getStreamErrorBody(response: Response): Promise<Record<string, unknown>> {
  try {
    const body: unknown = await response.json()
    return typeof body === 'object' && body !== null
      ? (body as Record<string, unknown>)
      : { error: String(body) }
  } catch {
    return { error: await response.text() }
  }
}

async function requireAssistantTurnStreamBody(
  response: Response,
): Promise<ReadableStream<Uint8Array>> {
  if (!response.ok) {
    // A dead session must end the stream here, not leave the assistant
    // retrying against a door that is closed.
    reportUnauthorizedResponse(response.status)
    throw new ApiError(response.status, await getStreamErrorBody(response))
  }
  if (!response.body) {
    throw new Error('Assistant stream response has no body')
  }
  return response.body
}

async function readAssistantTurnStream(
  response: Response,
  onActivity?: AssistantTurnActivityHandler,
): Promise<AssistantTurnExecutionResponse> {
  const reader = (await requireAssistantTurnStreamBody(response)).getReader()
  const decoder = new TextDecoder()
  let buffer = ''

  for (;;) {
    const { value, done } = await reader.read()
    buffer += decoder.decode(value ?? new Uint8Array(), { stream: !done })
    const blocks = buffer.split(/\r?\n\r?\n/)
    buffer = blocks.pop() ?? ''

    for (const block of blocks) {
      const completed = handleStreamEvent(parseSseBlock(block), onActivity)
      if (completed) return completed
    }

    if (done) break
  }

  const trailing = parseSseBlock(buffer)
  if (trailing?.type === 'completed') return trailing.detail
  if (trailing?.type === 'failed')
    throw new AssistantTurnFailedEventError(trailing.message, trailing.code)
  throw new Error('Assistant stream ended before completion')
}

function delay(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms))
}

const TERMINAL_ANSWER_STATES: ReadonlySet<string> = new Set(['completed', 'recovered'])
const TERMINAL_NO_ANSWER_STATES: ReadonlySet<string> = new Set(['failed', 'canceled'])

const QUERY_EXECUTION_ID_HEADER = 'x-ironrag-query-execution-id'
const UUID_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/

type DurableAssistantTurn =
  | { kind: 'answer'; detail: AssistantTurnExecutionResponse }
  | { kind: 'pending' }
  | { kind: 'no-answer' }

function isTerminalExecutionState(state: string): boolean {
  return TERMINAL_ANSWER_STATES.has(state) || TERMINAL_NO_ANSWER_STATES.has(state)
}

function readQueryExecutionId(response: Response): string | null {
  const raw = response.headers.get(QUERY_EXECUTION_ID_HEADER)?.trim().toLowerCase()
  if (!raw || !UUID_PATTERN.test(raw)) return null
  return raw
}

function matchDurableAssistantTurn(
  detail: AssistantTurnExecutionResponse,
  sessionId: string,
  executionId: string,
): DurableAssistantTurn {
  const execution = detail.execution
  if (execution.id !== executionId || execution.conversationId !== sessionId) {
    return { kind: 'no-answer' }
  }
  const requestTurn = detail.requestTurn
  const requestTurnId = execution.requestTurnId
  if (
    !requestTurnId ||
    requestTurn?.id !== requestTurnId ||
    requestTurn.conversationId !== sessionId ||
    requestTurn.turnKind !== 'user'
  ) {
    return { kind: 'no-answer' }
  }
  const state = execution.lifecycleState
  const responseTurn = detail.responseTurn
  const responseTurnId = execution.responseTurnId
  const responseLinked =
    !!responseTurnId &&
    responseTurn?.id === responseTurnId &&
    responseTurn.turnKind === 'assistant' &&
    responseTurn.conversationId === sessionId &&
    responseTurn.executionId === executionId &&
    responseTurn.contentText.trim().length > 0
  if (!responseLinked) {
    return isTerminalExecutionState(state) ? { kind: 'no-answer' } : { kind: 'pending' }
  }
  if (TERMINAL_ANSWER_STATES.has(state)) return { detail, kind: 'answer' }
  if (TERMINAL_NO_ANSWER_STATES.has(state)) return { kind: 'no-answer' }
  return { kind: 'pending' }
}

async function recoverAssistantExecution(
  sessionId: string,
  executionId: string,
): Promise<AssistantTurnExecutionResponse | null> {
  const deadline = Date.now() + STREAM_RECOVERY_TIMEOUT_MS
  const signal = AbortSignal.timeout(STREAM_RECOVERY_TIMEOUT_MS)
  let observed = false
  while (!signal.aborted && Date.now() < deadline) {
    let detail: AssistantTurnExecutionResponse
    try {
      detail = await queryApi.getExecution(executionId, signal)
    } catch (error: unknown) {
      if (signal.aborted) return null
      // The stream headers can arrive before the execution row is committed.
      if (!observed && error instanceof ApiError && error.status === 404) {
        await delay(STREAM_RECOVERY_INTERVAL_MS)
        continue
      }
      throw error
    }
    observed = true
    const match = matchDurableAssistantTurn(detail, sessionId, executionId)
    if (match.kind === 'answer') return match.detail
    // A terminal turn that does not carry the linked answer cannot be resolved
    // by waiting; surface the transport failure.
    if (match.kind === 'no-answer') return null
    await delay(STREAM_RECOVERY_INTERVAL_MS)
  }
  return null
}

/**
 * A turn the server accepted can lose its stream in the transport, before or
 * after the first activity frame, and the browser then cannot tell whether the
 * turn ran. Recovery is therefore allowed for any failure after an accepted
 * HTTP response, but never for a rejection the server actually sent and never
 * for a deliberate backend failure event.
 */
function canRecoverAssistantTurn(error: unknown): boolean {
  return !(error instanceof AssistantTurnFailedEventError) && !(error instanceof ApiError)
}

async function createAssistantTurnStreamRequest(
  sessionId: string,
  contentText: string,
  onAccepted: (executionId: string) => void,
  onActivity?: AssistantTurnActivityHandler,
): Promise<AssistantTurnExecutionResponse> {
  const response = await fetch(`/v1/query/sessions/${sessionId}/turns`, {
    body: JSON.stringify({ contentText }),
    credentials: 'include',
    headers: {
      Accept: 'text/event-stream',
      'Content-Type': 'application/json',
    },
    method: 'POST',
    signal: AbortSignal.timeout(TURN_TIMEOUT_MS),
  })
  const executionId = readQueryExecutionId(response)
  if (response.ok && executionId) onAccepted(executionId)
  return readAssistantTurnStream(response, onActivity)
}

export const queryApi = {
  listSessions: (params: { workspaceId: string; libraryId: string }) =>
    Query.listQuerySessions({ path: { libraryId: params.libraryId } }).then(
      (result): AssistantSessionListItem[] => unwrap(result).items,
    ),
  createSession: (_workspaceId: string, libraryId: string) =>
    Query.createQuerySession({ path: { libraryId }, body: {} }).then(
      (result): AssistantSessionListItem => unwrap(result),
    ),
  renameSession: (sessionId: string, title: string) =>
    Query.renameQuerySession({ body: { title }, path: { sessionId } }).then(
      (result): AssistantSessionListItem => unwrap(result),
    ),
  deleteSession: (sessionId: string) =>
    Query.deleteQuerySession({ path: { sessionId } }).then((result) => {
      unwrap(result)
    }),
  getSession: (sessionId: string) =>
    Query.getQuerySession({ path: { sessionId } }).then((result): AssistantHydratedConversation =>
      unwrap(result),
    ),
  createTurnStream: async (
    sessionId: string,
    contentText: string,
    onActivity?: AssistantTurnActivityHandler,
    onAccepted?: (executionId: string) => void,
  ) => {
    let acceptedExecutionId: string | null = null
    try {
      return await createAssistantTurnStreamRequest(
        sessionId,
        contentText,
        (executionId) => {
          acceptedExecutionId = executionId
          onAccepted?.(executionId)
        },
        onActivity,
      )
    } catch (error: unknown) {
      // Without the server-owned execution handle the browser cannot prove which
      // submission the durable answer belongs to, so it reports the failure.
      if (!acceptedExecutionId || !canRecoverAssistantTurn(error)) {
        throw error
      }
      const recovered = await recoverAssistantExecution(sessionId, acceptedExecutionId)
      if (recovered) return recovered
      throw error
    }
  },
  getExecution: (executionId: string, signal?: AbortSignal) =>
    Query.getQueryExecution({ path: { executionId }, signal: signal ?? null }).then(
      (result): AssistantExecutionDetail => unwrap(result),
    ),
  getExecutionLlmContext: (executionId: string) =>
    Query.getQueryExecutionLlmContext({ path: { executionId } }).then(
      (result): LlmContextDebugResponse => unwrap(result),
    ),
  getAssistantSystemPrompt: (libraryId?: string) =>
    Query.getAssistantSystemPrompt({
      query: libraryId !== undefined ? { libraryId } : {},
    }).then((result): AssistantSystemPromptResponse => unwrap(result)),
}
