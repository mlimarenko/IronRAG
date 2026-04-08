import { apiFetch } from "./client";

type ListAuditEventsParams = {
  workspaceId?: string;
  libraryId?: string;
  search?: string;
  surfaceKind?: string;
  resultKind?: string;
  limit?: number;
  offset?: number;
  internal?: boolean;
};

function buildQuery(params: Record<string, string | number | boolean | undefined>) {
  const searchParams = new URLSearchParams();
  for (const [key, value] of Object.entries(params)) {
    if (value === undefined || value === "") {
      continue;
    }
    searchParams.set(key, String(value));
  }

  const query = searchParams.toString();
  return query ? `?${query}` : "";
}

export const adminApi = {
  // Tokens
  listTokens: () => apiFetch<any>("/iam/tokens"),
  mintToken: (label: string) => apiFetch<any>("/iam/tokens", { method: "POST", body: JSON.stringify({ label }) }),
  revokeToken: (principalId: string) => apiFetch<void>(`/iam/tokens/${principalId}/revoke`, { method: "POST" }),

  // AI
  listProviders: () => apiFetch<any>("/ai/providers"),
  listModels: () => apiFetch<any>("/ai/models"),
  listCredentials: () => apiFetch<any>("/ai/credentials"),
  createCredential: (data: any) => apiFetch<any>("/ai/credentials", { method: "POST", body: JSON.stringify(data) }),
  listLibraryBindings: (libraryId: string) => apiFetch<any>(`/ai/libraries/${libraryId}/bindings`),
  createLibraryBinding: (data: any) => apiFetch<any>("/ai/library-bindings", { method: "POST", body: JSON.stringify(data) }),
  updateLibraryBinding: (bindingId: string, data: any) => apiFetch<any>(`/ai/library-bindings/${bindingId}`, { method: "PUT", body: JSON.stringify(data) }),
  listModelPresets: () => apiFetch<any>("/ai/model-presets"),
  createModelPreset: (data: any) => apiFetch<any>("/ai/model-presets", { method: "POST", body: JSON.stringify(data) }),
  listPrices: () => apiFetch<any>("/ai/prices"),
  createPriceOverride: (data: any) => apiFetch<any>("/ai/prices", { method: "POST", body: JSON.stringify(data) }),

  // Ops
  getAdminSurface: () => apiFetch<any>("/admin/surface"),

  // Audit
  listAuditEvents: (params: ListAuditEventsParams = {}) =>
    apiFetch<any>(
      `/audit/events${buildQuery({
        workspaceId: params.workspaceId,
        libraryId: params.libraryId,
        search: params.search,
        surfaceKind: params.surfaceKind,
        resultKind: params.resultKind,
        limit: params.limit,
        offset: params.offset,
        internal: params.internal,
      })}`,
    ),

  // Catalog
  listWorkspaces: () => apiFetch<any>("/catalog/workspaces"),
  createWorkspace: (name: string) => apiFetch<any>("/catalog/workspaces", { method: "POST", body: JSON.stringify({ displayName: name }) }),
  createLibrary: (workspaceId: string, name: string) =>
    apiFetch<any>(`/catalog/workspaces/${workspaceId}/libraries`, { method: "POST", body: JSON.stringify({ displayName: name }) }),
};
