import { Query } from "./generated";
import { ApiError, unwrap } from "./runtime";
import type {
  AssistantExecutionDetail,
  AssistantHydratedConversation,
  AssistantSessionListItem,
  AssistantSystemPromptResponse,
  LlmContextSnapshot,
  QueryConversation,
} from "./generated";
import type { AssistantAgentActivityEvent } from "@/shared/types";

export type AssistantTurnExecutionResponse = AssistantExecutionDetail;
export type LlmContextDebugResponse = LlmContextSnapshot;

/** Backend agent turns are capped at 180s; browser budgets leave shutdown headroom. */
const TURN_TIMEOUT_MS = 195_000;
const STREAM_RECOVERY_TIMEOUT_MS = 195_000;
const STREAM_RECOVERY_INTERVAL_MS = 1_000;

export type AssistantTurnStreamEvent =
  | { type: "activity"; event: AssistantAgentActivityEvent }
  | { type: "completed"; detail: AssistantTurnExecutionResponse }
  | { type: "failed"; message: string };

type AssistantTurnActivityHandler = (event: AssistantAgentActivityEvent) => void;

class AssistantTurnFailedEventError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "AssistantTurnFailedEventError";
  }
}

function parseSseBlock(block: string): AssistantTurnStreamEvent | null {
  const data = block
    .split(/\r?\n/)
    .filter((line) => line.startsWith("data:"))
    .map((line) => line.slice(5).trimStart())
    .join("\n")
    .trim();
  if (!data) return null;
  return JSON.parse(data) as AssistantTurnStreamEvent;
}

async function readAssistantTurnStream(
  response: Response,
  onActivity?: AssistantTurnActivityHandler,
): Promise<AssistantTurnExecutionResponse> {
  if (!response.ok) {
    let body: unknown;
    try {
      body = await response.json();
    } catch {
      body = { error: await response.text() };
    }
    throw new ApiError(
      response.status,
      typeof body === "object" && body !== null
        ? (body as Record<string, unknown>)
        : { error: String(body) },
    );
  }
  if (!response.body) {
    throw new Error("Assistant stream response has no body");
  }

  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";

  for (;;) {
    const { value, done } = await reader.read();
    buffer += decoder.decode(value ?? new Uint8Array(), { stream: !done });
    const blocks = buffer.split(/\r?\n\r?\n/);
    buffer = blocks.pop() ?? "";

    for (const block of blocks) {
      const event = parseSseBlock(block);
      if (!event) continue;
      if (event.type === "activity") {
        onActivity?.(event.event);
        continue;
      }
      if (event.type === "completed") return event.detail;
      throw new AssistantTurnFailedEventError(event.message);
    }

    if (done) break;
  }

  const trailing = parseSseBlock(buffer);
  if (trailing?.type === "completed") return trailing.detail;
  if (trailing?.type === "failed") throw new AssistantTurnFailedEventError(trailing.message);
  throw new Error("Assistant stream ended before completion");
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function isRecoverableAssistantStreamError(error: unknown): boolean {
  if (error instanceof AssistantTurnFailedEventError) return false;
  const lower = errorMessage(error).toLowerCase();
  return (
    lower.includes("input stream") ||
    lower.includes("networkerror") ||
    lower.includes("network error") ||
    lower.includes("failed to fetch") ||
    lower.includes("load failed") ||
    lower.includes("body stream") ||
    lower.includes("stream ended before completion") ||
    lower.includes("terminated") ||
    lower.includes("abort") ||
    lower.includes("timeout")
  );
}

function delay(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function latestAssistantExecutionAfterQuestion(
  conversation: AssistantHydratedConversation,
  questionText: string,
  minimumQuestionIndex: number,
): string | null {
  const normalizedQuestion = questionText.trim();
  let lastQuestionIndex = -1;
  conversation.messages.forEach((message, index) => {
    if (
      index >= minimumQuestionIndex &&
      message.role === "user" &&
      message.content.trim() === normalizedQuestion
    ) {
      lastQuestionIndex = index;
    }
  });
  if (lastQuestionIndex < 0) return null;

  for (let index = conversation.messages.length - 1; index > lastQuestionIndex; index -= 1) {
    const message = conversation.messages[index];
    if (message.role === "assistant" && message.executionId) {
      return message.executionId;
    }
  }
  return null;
}

async function recoverAssistantTurnFromDurableSession(
  sessionId: string,
  questionText: string,
  minimumQuestionIndex: number,
): Promise<AssistantTurnExecutionResponse | null> {
  const deadline = Date.now() + STREAM_RECOVERY_TIMEOUT_MS;
  while (Date.now() < deadline) {
    const conversation = await queryApi.getSession(sessionId);
    const executionId = latestAssistantExecutionAfterQuestion(
      conversation,
      questionText,
      minimumQuestionIndex,
    );
    if (executionId) {
      return queryApi.getExecution(executionId);
    }
    await delay(STREAM_RECOVERY_INTERVAL_MS);
  }
  return null;
}

export const queryApi = {
  listSessions: (params: { workspaceId: string; libraryId: string }) =>
    Query.listQuerySessions({ query: { libraryId: params.libraryId } }).then(
      (result): AssistantSessionListItem[] => unwrap(result),
    ),
  createSession: (workspaceId: string, libraryId: string) =>
    Query.createQuerySession({ body: { workspaceId, libraryId } }).then(
      (result): QueryConversation => unwrap(result),
    ),
  getSession: (sessionId: string) =>
    Query.getQuerySession({ path: { sessionId } }).then(
      (result): AssistantHydratedConversation => unwrap(result),
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
    onActivity?: AssistantTurnActivityHandler,
  ) => {
    let sawActivity = false;
    let recoveryMessageStartIndex: number | null;
    try {
      recoveryMessageStartIndex = (await queryApi.getSession(sessionId)).messages.length;
    } catch {
      recoveryMessageStartIndex = null;
    }
    const handleActivity: AssistantTurnActivityHandler = (event) => {
      sawActivity = true;
      onActivity?.(event);
    };
    try {
      const response = await fetch(`/v1/query/sessions/${sessionId}/turns`, {
        body: JSON.stringify({ contentText }),
        credentials: "include",
        headers: {
          Accept: "text/event-stream",
          "Content-Type": "application/json",
        },
        method: "POST",
        signal: AbortSignal.timeout(TURN_TIMEOUT_MS),
      });
      return await readAssistantTurnStream(response, handleActivity);
    } catch (error: unknown) {
      if (
        recoveryMessageStartIndex === null ||
        !sawActivity ||
        !isRecoverableAssistantStreamError(error)
      ) {
        throw error;
      }
      const recovered = await recoverAssistantTurnFromDurableSession(
        sessionId,
        contentText,
        recoveryMessageStartIndex,
      );
      if (recovered) return recovered;
      throw error;
    }
  },
  getExecution: (executionId: string) =>
    Query.getQueryExecution({ path: { executionId } }).then(
      (result): AssistantExecutionDetail => unwrap(result),
    ),
  getExecutionLlmContext: (executionId: string) =>
    Query.getQueryExecutionLlmContext({ path: { executionId } }).then(
      (result): LlmContextDebugResponse => unwrap(result),
    ),
  getAssistantSystemPrompt: (libraryId?: string) =>
    Query.getAssistantSystemPrompt({ query: { libraryId } }).then(
      (result): AssistantSystemPromptResponse => unwrap(result),
    ),
};
