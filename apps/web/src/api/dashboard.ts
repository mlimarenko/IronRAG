import { apiFetch } from "./client";
import type { RawOpsResponse } from "@/types/api-responses";

export const dashboardApi = {
  /**
   * Returns the library dashboard payload. The richer view model lives in
   * `DashboardPage`; the api layer treats it as opaque to avoid duplicating
   * the schema in two places.
   */
  getLibraryDashboard: (libraryId: string) =>
    apiFetch<unknown>(`/ops/libraries/${libraryId}/dashboard`),
  getLibraryState: (libraryId: string) =>
    apiFetch<RawOpsResponse>(`/ops/libraries/${libraryId}`),
};
