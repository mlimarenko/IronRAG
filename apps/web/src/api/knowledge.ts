import { apiFetch } from "./client";

/**
 * Knowledge endpoints return either a bare array or a paginated `{ items }`
 * envelope depending on the resource. Callers normalize the shape themselves,
 * so we expose a permissive structural type rather than enumerate every
 * resource projection.
 */
export type KnowledgeListResponse<T = unknown> =
  | T[]
  | {
      items?: T[];
      documents?: T[];
      [key: string]: unknown;
    };

export interface KnowledgeEntityDetailResponse {
  entity?: Record<string, unknown>;
  [key: string]: unknown;
}

export interface KnowledgeGraphTopologyResponse {
  documentLinks?: Array<Record<string, unknown>>;
  [key: string]: unknown;
}

export interface KnowledgeGraphWorkbenchResponse {
  [key: string]: unknown;
}

export interface KnowledgeLibrarySummaryResponse {
  [key: string]: unknown;
}

export const knowledgeApi = {
  getGraphWorkbench: (libraryId: string) =>
    apiFetch<KnowledgeGraphWorkbenchResponse>(`/knowledge/libraries/${libraryId}/graph-workbench`),
  getGraphTopology: (libraryId: string) =>
    apiFetch<KnowledgeGraphTopologyResponse>(`/knowledge/libraries/${libraryId}/graph-topology`),
  listEntities: (libraryId: string) =>
    apiFetch<KnowledgeListResponse>(`/knowledge/libraries/${libraryId}/entities`),
  getEntity: (libraryId: string, entityId: string) =>
    apiFetch<KnowledgeEntityDetailResponse>(`/knowledge/libraries/${libraryId}/entities/${entityId}`),
  listRelations: (libraryId: string) =>
    apiFetch<KnowledgeListResponse>(`/knowledge/libraries/${libraryId}/relations`),
  getLibrarySummary: (libraryId: string) =>
    apiFetch<KnowledgeLibrarySummaryResponse>(`/knowledge/libraries/${libraryId}/summary`),
  listDocuments: (libraryId: string) =>
    apiFetch<KnowledgeListResponse>(`/knowledge/libraries/${libraryId}/documents`),
};
