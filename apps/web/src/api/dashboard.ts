import { apiFetch } from "./client";

export const dashboardApi = {
  getLibraryDashboard: (libraryId: string) => apiFetch<any>(`/ops/libraries/${libraryId}/dashboard`),
  getLibraryState: (libraryId: string) => apiFetch<any>(`/ops/libraries/${libraryId}`),
};
