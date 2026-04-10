import { apiFetch } from "./client";
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
    contentText?: string;
    createdAt?: string;
  };
}

export interface QueryExecutionResponse {
  id?: string;
  state?: string;
  [key: string]: unknown;
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
  getExecution: (executionId: string) =>
    apiFetch<QueryExecutionResponse>(`/query/executions/${executionId}`),
};
