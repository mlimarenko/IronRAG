import { apiFetch } from "./client";
import type { AIScopeKind } from "@/types";
import type {
  RawProviderCatalogEntry,
  RawModelCatalogEntry,
  RawProviderCredentialResponse,
  RawModelPresetResponse,
  RawBindingAssignmentResponse,
  RawTokenResponse,
  RawTokenMintResponse,
  RawPricingResponse,
  RawAuditPageResponse,
} from "@/types/api-responses";
import type {
  CreateCredentialRequest,
  UpdateCredentialRequest,
  CreateBindingRequest,
  UpdateBindingRequest,
  CreateModelPresetRequest,
  UpdateModelPresetRequest,
  CreatePriceOverrideRequest,
} from "@/types/api-requests";

type ListAuditEventsParams = {
  workspaceId?: string;
  libraryId?: string;
  search?: string;
  surfaceKind?: string;
  resultKind?: string;
  limit?: number;
  offset?: number;
  internal?: boolean;
  includeAssistant?: boolean;
};

type AiScopeParams = {
  scopeKind?: AIScopeKind;
  workspaceId?: string;
  libraryId?: string;
};

type ListModelsParams = {
  providerCatalogId?: string;
  workspaceId?: string;
  libraryId?: string;
  credentialId?: string;
};

export interface AdminSurfaceResponse {
  workspaces?: unknown[];
  libraries?: unknown[];
  [key: string]: unknown;
}

export interface CatalogWorkspaceResponse {
  id: string;
  displayName?: string;
  createdAt?: string;
}

export interface CatalogLibraryResponse {
  id: string;
  workspaceId: string;
  displayName?: string;
  createdAt?: string;
}

export interface BindingValidationResponse {
  state?: string;
  checkedAt?: string;
  failureCode?: string;
  message?: string;
}

export interface MintTokenRequest {
  label: string;
  workspaceId?: string;
  expiresAt?: string;
  libraryIds?: string[];
  permissionKinds?: string[];
}

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
  listTokens: () => apiFetch<RawTokenResponse[]>("/iam/tokens"),
  mintToken: (request: MintTokenRequest) =>
    apiFetch<RawTokenMintResponse>("/iam/tokens", { method: "POST", body: JSON.stringify(request) }),
  revokeToken: (principalId: string) =>
    apiFetch<void>(`/iam/tokens/${principalId}/revoke`, { method: "POST" }),

  listProviders: () => apiFetch<RawProviderCatalogEntry[]>("/ai/providers"),
  listModels: (params: ListModelsParams = {}) =>
    apiFetch<RawModelCatalogEntry[]>(`/ai/models${buildQuery(params)}`),
  listCredentials: (params: AiScopeParams = {}) =>
    apiFetch<RawProviderCredentialResponse[]>(`/ai/credentials${buildQuery(params)}`),
  createCredential: (data: CreateCredentialRequest) =>
    apiFetch<RawProviderCredentialResponse>("/ai/credentials", {
      method: "POST",
      body: JSON.stringify(data),
    }),
  updateCredential: (credentialId: string, data: UpdateCredentialRequest) =>
    apiFetch<RawProviderCredentialResponse>(`/ai/credentials/${credentialId}`, {
      method: "PUT",
      body: JSON.stringify(data),
    }),
  listBindings: (params: Required<Pick<AiScopeParams, 'scopeKind'>> & AiScopeParams) =>
    apiFetch<RawBindingAssignmentResponse[]>(`/ai/bindings${buildQuery(params)}`),
  createBinding: (data: CreateBindingRequest) =>
    apiFetch<RawBindingAssignmentResponse>("/ai/bindings", {
      method: "POST",
      body: JSON.stringify(data),
    }),
  updateBinding: (bindingId: string, data: UpdateBindingRequest) =>
    apiFetch<RawBindingAssignmentResponse>(`/ai/bindings/${bindingId}`, {
      method: "PUT",
      body: JSON.stringify(data),
    }),
  deleteBinding: (bindingId: string) =>
    apiFetch<void>(`/ai/bindings/${bindingId}`, { method: "DELETE" }),
  validateBinding: (bindingId: string) =>
    apiFetch<BindingValidationResponse>(`/ai/bindings/${bindingId}/validate`, { method: "POST" }),
  listModelPresets: (params: AiScopeParams = {}) =>
    apiFetch<RawModelPresetResponse[]>(`/ai/model-presets${buildQuery(params)}`),
  createModelPreset: (data: CreateModelPresetRequest) =>
    apiFetch<RawModelPresetResponse>("/ai/model-presets", {
      method: "POST",
      body: JSON.stringify(data),
    }),
  updateModelPreset: (presetId: string, data: UpdateModelPresetRequest) =>
    apiFetch<RawModelPresetResponse>(`/ai/model-presets/${presetId}`, {
      method: "PUT",
      body: JSON.stringify(data),
    }),
  listPrices: () => apiFetch<RawPricingResponse[]>("/ai/prices"),
  createPriceOverride: (data: CreatePriceOverrideRequest) =>
    apiFetch<RawPricingResponse>("/ai/prices", {
      method: "POST",
      body: JSON.stringify(data),
    }),

  getAdminSurface: () => apiFetch<AdminSurfaceResponse>("/admin/surface"),

  listAuditEvents: (params: ListAuditEventsParams = {}) =>
    apiFetch<RawAuditPageResponse>(
      `/audit/events${buildQuery({
        workspaceId: params.workspaceId,
        libraryId: params.libraryId,
        search: params.search,
        surfaceKind: params.surfaceKind,
        resultKind: params.resultKind,
        limit: params.limit,
        offset: params.offset,
        internal: params.internal,
        includeAssistant: params.includeAssistant,
      })}`,
    ),

  listWorkspaces: () => apiFetch<CatalogWorkspaceResponse[]>("/catalog/workspaces"),
  listLibraries: (workspaceId: string) =>
    apiFetch<CatalogLibraryResponse[]>(`/catalog/workspaces/${workspaceId}/libraries`),
  createWorkspace: (name: string) =>
    apiFetch<CatalogWorkspaceResponse>("/catalog/workspaces", {
      method: "POST",
      body: JSON.stringify({ displayName: name }),
    }),
  createLibrary: (workspaceId: string, name: string) =>
    apiFetch<CatalogLibraryResponse>(`/catalog/workspaces/${workspaceId}/libraries`, {
      method: "POST",
      body: JSON.stringify({ displayName: name }),
    }),
};
