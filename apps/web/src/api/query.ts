import { apiFetch } from "./client";
import type {
  RawAssistantSession,
  RawAssistantMessage,
  RawAssistantTurnResponse,
} from "@/types/api-responses";

export interface AssistantHydratedConversationResponse {
  session: RawAssistantSession;
  messages: RawAssistantMessage[];
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
    apiFetch<AssistantHydratedConversationResponse>(`/query/sessions/${sessionId}`),
  createTurn: (sessionId: string, contentText: string) =>
    apiFetch<AssistantTurnExecutionResponse>(`/query/sessions/${sessionId}/turns`, {
      method: "POST",
      body: JSON.stringify({ contentText }),
    }),
  getExecution: (executionId: string) =>
    apiFetch<AssistantExecutionDetailResponse>(`/query/executions/${executionId}`),
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
