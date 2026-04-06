import { apiFetch } from "./client";

export const queryApi = {
  listSessions: (params: { workspaceId: string; libraryId: string }) => {
    const qs = new URLSearchParams({ workspaceId: params.workspaceId, libraryId: params.libraryId });
    return apiFetch<any>(`/query/sessions?${qs}`);
  },
  createSession: (workspaceId: string, libraryId: string) =>
    apiFetch<any>("/query/sessions", {
      method: "POST",
      body: JSON.stringify({ workspaceId, libraryId }),
    }),
  getSession: (sessionId: string) => apiFetch<any>(`/query/sessions/${sessionId}`),
  createTurn: (sessionId: string, contentText: string) =>
    apiFetch<any>(`/query/sessions/${sessionId}/turns`, {
      method: "POST",
      body: JSON.stringify({ contentText }),
    }),
  getExecution: (executionId: string) => apiFetch<any>(`/query/executions/${executionId}`),
};
