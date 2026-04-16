import { apiFetch, ApiError } from "./client";
import type {
  RawAssistantSession,
  RawAssistantMessage,
  RawAssistantTurnResponse,
} from "@/types/api-responses";

export interface AssistantSessionDetailResponse extends RawAssistantSession {
  messages?: RawAssistantMessage[];
}

export interface AssistantTurnExecutionResponse extends RawAssistantTurnResponse {
  responseTurn?: {
    id?: string;
    executionId?: string;
    contentText?: string;
    createdAt?: string;
  };
}

export interface QueryExecutionResponse {
  id?: string;
  state?: string;
  [key: string]: unknown;
}

export interface LlmIterationDebugResponse {
  iteration: number;
  providerKind: string;
  modelName: string;
  requestMessages: Array<{
    role: string;
    content?: string | null;
    toolCalls?: Array<{
      id: string;
      name: string;
      argumentsJson: string;
    }>;
    toolCallId?: string | null;
    name?: string | null;
  }>;
  responseText: string | null;
  responseToolCalls: Array<{
    id: string;
    name: string;
    argumentsJson: string;
    resultText: string | null;
    isError: boolean;
  }>;
  usage: unknown;
}

export interface AssistantSystemPromptResponse {
  template: string;
  rendered: string | null;
  libraryId: string | null;
}

export interface LlmContextDebugResponse {
  executionId: string;
  libraryId: string;
  question: string;
  totalIterations: number;
  iterations: LlmIterationDebugResponse[];
  finalAnswer: string | null;
  capturedAt: string;
}

export const queryApi = {
  listSessions: (params: { workspaceId: string; libraryId: string }) => {
    const qs = new URLSearchParams({
      workspaceId: params.workspaceId,
      libraryId: params.libraryId,
    });
    return apiFetch<RawAssistantSession[]>(`/query/sessions?${qs}`);
  },
  createSession: (workspaceId: string, libraryId: string) =>
    apiFetch<RawAssistantSession>("/query/sessions", {
      method: "POST",
      body: JSON.stringify({ workspaceId, libraryId }),
    }),
  getSession: (sessionId: string) =>
    apiFetch<AssistantSessionDetailResponse>(`/query/sessions/${sessionId}`),
  createTurn: (sessionId: string, contentText: string) =>
    apiFetch<AssistantTurnExecutionResponse>(`/query/sessions/${sessionId}/turns`, {
      method: "POST",
      body: JSON.stringify({ contentText }),
    }),
  /// Open the SSE stream variant of `createTurn`. The backend picks
  /// this branch based on `Accept: text/event-stream` and emits
  /// `runtime` / `delta` / `completed` / `error` frames. Deltas arrive
  /// incrementally; the returned promise resolves on the `completed`
  /// frame with the same shape as the non-streaming response, so call
  /// sites can treat the final payload identically.
  createTurnStream: async (
    sessionId: string,
    contentText: string,
    handlers: {
      onDelta?: (delta: string) => void;
      onRuntime?: (runtime: unknown) => void;
      onToolCallStarted?: (event: {
        iteration: number;
        callId: string;
        name: string;
        argumentsPreview: string;
      }) => void;
      onToolCallCompleted?: (event: {
        iteration: number;
        callId: string;
        name: string;
        isError: boolean;
        resultPreview: string;
      }) => void;
    } = {},
  ): Promise<AssistantTurnExecutionResponse> => {
    const res = await fetch(`/v1/query/sessions/${sessionId}/turns`, {
      method: "POST",
      credentials: "include",
      headers: {
        "Content-Type": "application/json",
        Accept: "text/event-stream",
      },
      body: JSON.stringify({ contentText }),
    });
    if (!res.ok || !res.body) {
      const body = (await res.json().catch(() => ({}))) as Record<string, unknown>;
      throw new ApiError(res.status, body);
    }
    const reader = res.body.getReader();
    const decoder = new TextDecoder();
    let buffer = "";
    let completed: AssistantTurnExecutionResponse | null = null;
    let streamError: { error: string; errorKind?: string } | null = null;

    const handleFrame = (event: string, dataRaw: string) => {
      if (!dataRaw) return;
      let parsed: unknown;
      try {
        parsed = JSON.parse(dataRaw);
      } catch {
        return;
      }
      if (event === "delta") {
        const payload = parsed as { delta?: string };
        if (typeof payload.delta === "string") handlers.onDelta?.(payload.delta);
      } else if (event === "runtime") {
        const payload = parsed as { runtime?: unknown };
        handlers.onRuntime?.(payload.runtime);
      } else if (event === "tool_call_started") {
        handlers.onToolCallStarted?.(parsed as {
          iteration: number;
          callId: string;
          name: string;
          argumentsPreview: string;
        });
      } else if (event === "tool_call_completed") {
        handlers.onToolCallCompleted?.(parsed as {
          iteration: number;
          callId: string;
          name: string;
          isError: boolean;
          resultPreview: string;
        });
      } else if (event === "completed") {
        completed = parsed as AssistantTurnExecutionResponse;
      } else if (event === "error") {
        streamError = parsed as { error: string; errorKind?: string };
      }
    };

    // eslint-disable-next-line no-constant-condition
    while (true) {
      const { value, done } = await reader.read();
      if (done) break;
      buffer += decoder.decode(value, { stream: true });
      let separator: number;
      while ((separator = buffer.indexOf("\n\n")) !== -1) {
        const frame = buffer.slice(0, separator);
        buffer = buffer.slice(separator + 2);
        let event = "message";
        let data = "";
        for (const line of frame.split("\n")) {
          if (line.startsWith("event:")) event = line.slice(6).trim();
          else if (line.startsWith("data:")) data += line.slice(5).trim();
        }
        handleFrame(event, data);
      }
    }

    if (streamError) {
      const err = streamError as { error: string; errorKind?: string };
      throw new Error(err.error);
    }
    if (!completed) {
      throw new Error("assistant stream ended without a completed frame");
    }
    return completed;
  },
  getExecution: (executionId: string) =>
    apiFetch<QueryExecutionResponse>(`/query/executions/${executionId}`),
  getExecutionLlmContext: (executionId: string) =>
    apiFetch<LlmContextDebugResponse>(
      `/query/executions/${executionId}/llm-context`,
    ),
  getAssistantSystemPrompt: (libraryId?: string) => {
    const path = libraryId
      ? `/query/assistant/system-prompt?libraryId=${encodeURIComponent(libraryId)}`
      : "/query/assistant/system-prompt";
    return apiFetch<AssistantSystemPromptResponse>(path);
  },
};
