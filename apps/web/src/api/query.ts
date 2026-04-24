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

export interface AssistantExecutionSummary {
  id?: string;
  runtimeExecutionId?: string | null;
  lifecycleState?: string;
  activeStage?: string | null;
  failureCode?: string | null;
  completedAt?: string | null;
}

export interface AssistantExecutionDetailResponse
  extends AssistantTurnExecutionResponse {
  execution?: AssistantExecutionSummary;
}

export interface RuntimeExecutionResponse {
  executionId?: string;
  ownerKind?: string;
  ownerId?: string;
  lifecycleState?: string;
  activeStage?: string | null;
  failureCode?: string | null;
  completedAt?: string | null;
}

export interface AssistantRuntimeProgress {
  runtimeExecutionId?: string;
  lifecycleState?: string;
  activeStage?: string | null;
  completedAt?: string | null;
  failureCode?: string | null;
}

type AssistantTurnStreamHandlers = {
  onDelta?: (delta: string) => void;
  onRuntime?: (runtime: AssistantRuntimeProgress) => void;
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
};

const QUERY_EXECUTION_TERMINAL_STATES = new Set(["completed", "failed", "canceled"]);

function isExecutionTerminal(execution?: AssistantExecutionSummary): boolean {
  if (!execution) return false;
  if (typeof execution.completedAt === "string" && execution.completedAt.length > 0) {
    return true;
  }
  return QUERY_EXECUTION_TERMINAL_STATES.has(
    (execution.lifecycleState ?? "").trim().toLowerCase(),
  );
}

function delay(ms: number): Promise<void> {
  return new Promise((resolve) => {
    window.setTimeout(resolve, ms);
  });
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

/// Thrown by `createTurnStream` when the SSE transport itself is
/// unavailable — the request was rejected before the backend could
/// emit any frame. Canonical triggers: Firefox Tracking Protection /
/// uBlock / Privacy Badger, corporate TLS-inspection proxies, lost
/// connectivity at TCP level. Resilient call-sites use
/// `createTurnWithFallback` which catches this and retries via the
/// non-SSE POST path.
export class SseTransportUnavailableError extends Error {
  override readonly cause?: unknown;
  constructor(cause: unknown) {
    super("SSE transport unavailable");
    this.name = "SseTransportUnavailableError";
    this.cause = cause;
  }
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
  /// Resilient turn entrypoint. Prefers the SSE stream for real-time
  /// progress; if the stream transport is unavailable (ad-blocker,
  /// Firefox Tracking Protection, corporate proxy, misbehaving
  /// intermediary), falls back to the non-SSE POST and returns the
  /// final payload without incremental frames. The backend Accept
  /// dispatcher accepts both shapes.
  createTurnWithFallback: async (
    sessionId: string,
    contentText: string,
    handlers: AssistantTurnStreamHandlers = {},
  ): Promise<AssistantTurnExecutionResponse> => {
    try {
      return await queryApi.createTurnStream(sessionId, contentText, handlers);
    } catch (err) {
      if (err instanceof SseTransportUnavailableError) {
        // eslint-disable-next-line no-console
        console.warn(
          "SSE transport unavailable, falling back to non-SSE POST",
          err.cause,
        );
        return queryApi.createTurn(sessionId, contentText);
      }
      throw err;
    }
  },
  /// Open the SSE stream variant of `createTurn`. The backend picks
  /// this branch based on `Accept: text/event-stream` and emits
  /// `runtime` / `delta` / `completed` / `error` frames. Deltas arrive
  /// incrementally; the returned promise resolves on the `completed`
  /// frame with the same shape as the non-streaming response, so call
  /// sites can treat the final payload identically.
  ///
  /// Throws `SseTransportUnavailableError` if the transport itself
  /// failed before the backend could return any frame (network error,
  /// client-side block). Any later stream error — malformed frame,
  /// backend error event, truncated stream — is thrown as a generic
  /// Error; the turn has already been accepted server-side and any
  /// retry would create a duplicate turn, so callers must surface the
  /// error rather than transparently retry.
  createTurnStream: async (
    sessionId: string,
    contentText: string,
    handlers: AssistantTurnStreamHandlers = {},
  ): Promise<AssistantTurnExecutionResponse> => {
    let res: Response;
    try {
      res = await fetch(`/v1/query/sessions/${sessionId}/turns`, {
        method: "POST",
        credentials: "include",
        headers: {
          "Content-Type": "application/json",
          Accept: "text/event-stream",
        },
        body: JSON.stringify({ contentText }),
      });
    } catch (fetchErr) {
      // fetch() rejects only on network-layer failure (DNS, refused,
      // CORS preflight, client-side block). The request never reached
      // the backend, so a non-SSE retry is safe and won't duplicate
      // the turn.
      throw new SseTransportUnavailableError(fetchErr);
    }
    if (!res.body) {
      // 200 with an empty body is also transport failure — an
      // intermediary stripped the stream before any frame arrived.
      throw new SseTransportUnavailableError(
        new Error(`response has no body (status ${res.status})`),
      );
    }
    if (!res.ok) {
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
        const payload = parsed as { runtime?: AssistantRuntimeProgress };
        if (payload.runtime && typeof payload.runtime === "object") {
          handlers.onRuntime?.(payload.runtime);
        }
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
    apiFetch<AssistantExecutionDetailResponse>(`/query/executions/${executionId}`),
  getRuntimeExecution: (runtimeExecutionId: string) =>
    apiFetch<RuntimeExecutionResponse>(`/runtime/executions/${runtimeExecutionId}`),
  recoverTurnAfterStreamFailure: async (
    runtimeExecutionId: string,
    timeoutMs = 30000,
  ): Promise<AssistantExecutionDetailResponse | null> => {
    let runtimeExecution: RuntimeExecutionResponse;
    try {
      runtimeExecution = await queryApi.getRuntimeExecution(runtimeExecutionId);
    } catch (err) {
      if (err instanceof ApiError && err.status === 404) return null;
      throw err;
    }
    if (runtimeExecution.ownerKind !== "query_execution" || !runtimeExecution.ownerId) {
      return null;
    }

    const deadline = Date.now() + timeoutMs;
    let backoffMs = 250;
    while (Date.now() <= deadline) {
      try {
        const detail = await queryApi.getExecution(runtimeExecution.ownerId);
        if (detail.responseTurn?.id || detail.responseTurn?.contentText) {
          return detail;
        }
        if (isExecutionTerminal(detail.execution)) {
          return detail;
        }
      } catch (err) {
        if (!(err instanceof ApiError) || err.status !== 404) {
          throw err;
        }
      }
      await delay(backoffMs);
      backoffMs = Math.min(backoffMs * 2, 1500);
    }
    return null;
  },
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
