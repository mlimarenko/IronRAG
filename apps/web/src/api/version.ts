import { apiFetch } from "./client";

export type ReleaseUpdateStatus = "up_to_date" | "update_available" | "unknown";

export interface ReleaseUpdateResponse {
  status: ReleaseUpdateStatus;
  currentVersion: string;
  latestVersion: string | null;
  releaseUrl: string | null;
  repositoryUrl: string;
  checkedAt: string;
}

export const versionApi = {
  getReleaseUpdate: () => apiFetch<ReleaseUpdateResponse>("/version/update"),
};
