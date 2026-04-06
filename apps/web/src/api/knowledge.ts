import { apiFetch } from "./client";

export const knowledgeApi = {
  getGraphWorkbench: (libraryId: string) => apiFetch<any>(`/knowledge/libraries/${libraryId}/graph-workbench`),
  getGraphTopology: (libraryId: string) => apiFetch<any>(`/knowledge/libraries/${libraryId}/graph-topology`),
  listEntities: (libraryId: string) => apiFetch<any>(`/knowledge/libraries/${libraryId}/entities`),
  getEntity: (libraryId: string, entityId: string) => apiFetch<any>(`/knowledge/libraries/${libraryId}/entities/${entityId}`),
  listRelations: (libraryId: string) => apiFetch<any>(`/knowledge/libraries/${libraryId}/relations`),
  getLibrarySummary: (libraryId: string) => apiFetch<any>(`/knowledge/libraries/${libraryId}/summary`),
  listDocuments: (libraryId: string) => apiFetch<any>(`/knowledge/libraries/${libraryId}/documents`),
};
