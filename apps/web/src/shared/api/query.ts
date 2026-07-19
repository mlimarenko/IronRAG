import { Query } from './generated'
import { ApiError, unwrap } from './runtime'
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
  | { type: 'failed'; message: string }

type AssistantTurnActivityHandler = (event: AssistantAgentActivityEvent) => void

class AssistantTurnFailedEventError extends Error {
  constructor(message: string) {
    super(message)
    this.name = 'AssistantTurnFailedEventError'
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
  throw new AssistantTurnFailedEventError(event.message)
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

async function ensureAssistantTurnStreamResponse(response: Response): Promise<void> {
  if (!response.ok) {
    throw new ApiError(response.status, await getStreamErrorBody(response))
  }
  if (!response.body) {
    throw new Error('Assistant stream response has no body')
  }
}

async function readAssistantTurnStream(
  response: Response,
  onActivity?: AssistantTurnActivityHandler,
): Promise<AssistantTurnExecutionResponse> {
  await ensureAssistantTurnStreamResponse(response)

  const reader = response.body.getReader()
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
  if (trailing?.type === 'failed') throw new AssistantTurnFailedEventError(trailing.message)
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
  createTurn: (sessionId: string, contentText: string) =>
    Query.createQuerySessionTurn({
      body: { contentText },
      path: { sessionId },
      signal: AbortSignal.timeout(TURN_TIMEOUT_MS),
    }).then((result): AssistantTurnExecutionResponse => unwrap(result)),
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
