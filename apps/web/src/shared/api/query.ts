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

function isRecoverableAssistantStreamError(error: unknown): boolean {
  return !(error instanceof AssistantTurnFailedEventError) && !(error instanceof ApiError)
}

function delay(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms))
}

function latestAssistantExecutionAfterQuestion(
  conversation: AssistantHydratedConversation,
  questionText: string,
  minimumQuestionIndex: number,
): string | null {
  const normalizedQuestion = questionText.trim()
  let lastQuestionIndex = -1
  conversation.messages.forEach((message, index) => {
    if (
      index >= minimumQuestionIndex &&
      message.role === 'user' &&
      message.content.trim() === normalizedQuestion
    ) {
      lastQuestionIndex = index
    }
  })
  if (lastQuestionIndex < 0) return null

  for (let index = conversation.messages.length - 1; index > lastQuestionIndex; index -= 1) {
    const message = conversation.messages[index]
    if (!message) continue
    if (message.role === 'assistant' && message.executionId) {
      return message.executionId
    }
  }
  return null
}

async function recoverAssistantTurnFromDurableSession(
  sessionId: string,
  questionText: string,
  minimumQuestionIndex: number,
): Promise<AssistantTurnExecutionResponse | null> {
  const deadline = Date.now() + STREAM_RECOVERY_TIMEOUT_MS
  while (Date.now() < deadline) {
    const conversation = await queryApi.getSession(sessionId)
    const executionId = latestAssistantExecutionAfterQuestion(
      conversation,
      questionText,
      minimumQuestionIndex,
    )
    if (executionId) {
      return queryApi.getExecution(executionId)
    }
    await delay(STREAM_RECOVERY_INTERVAL_MS)
  }
  return null
}

function canRecoverAssistantTurn(error: unknown, sawActivity: boolean): boolean {
  return sawActivity && isRecoverableAssistantStreamError(error)
}

async function createAssistantTurnStreamRequest(
  sessionId: string,
  contentText: string,
  onActivity: AssistantTurnActivityHandler,
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
    recoveryMessageStartIndex: number,
    onActivity?: AssistantTurnActivityHandler,
  ) => {
    let sawActivity = false
    const handleActivity: AssistantTurnActivityHandler = (event) => {
      sawActivity = true
      onActivity?.(event)
    }
    try {
      return await createAssistantTurnStreamRequest(sessionId, contentText, handleActivity)
    } catch (error: unknown) {
      if (!canRecoverAssistantTurn(error, sawActivity)) {
        throw error
      }
      const recovered = await recoverAssistantTurnFromDurableSession(
        sessionId,
        contentText,
        recoveryMessageStartIndex,
      )
      if (recovered) return recovered
      throw error
    }
  },
  getExecution: (executionId: string) =>
    Query.getQueryExecution({ path: { executionId } }).then((result): AssistantExecutionDetail =>
      unwrap(result),
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
