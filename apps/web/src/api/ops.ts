import { apiFetch } from "./client";

/**
 * Canonical async-operation polling payload returned by
 * `GET /v1/ops/operations/{id}`. Any batch endpoint (batch rerun, batch
 * delete, future batch annotate …) creates a parent `ops_async_operation`
 * row and spawns per-subject children linked via `parentAsyncOperationId`.
 * Pollers render `progress.completed / progress.total` and stop when
 * `status` transitions to `ready`, `failed`, `canceled`, or `superseded`.
 */
export type AsyncOperationStatus =
  | "accepted"
  | "processing"
  | "ready"
  | "failed"
  | "superseded"
  | "canceled";

export interface AsyncOperationProgress {
  total: number;
  completed: number;
  failed: number;
  inFlight: number;
}

export interface AsyncOperationDetail {
  id: string;
  workspaceId: string;
  libraryId: string | null;
  operationKind: string;
  status: AsyncOperationStatus;
  surfaceKind: string | null;
  subjectKind: string | null;
  subjectId: string | null;
  parentAsyncOperationId: string | null;
  failureCode: string | null;
  createdAt: string;
  completedAt: string | null;
  progress: AsyncOperationProgress;
}

/** Terminal statuses — polling must stop on any of these. */
export const ASYNC_OPERATION_TERMINAL_STATES: ReadonlySet<AsyncOperationStatus> =
  new Set(["ready", "failed", "canceled", "superseded"]);

export const opsApi = {
  getAsyncOperation: (operationId: string) =>
    apiFetch<AsyncOperationDetail>(`/ops/operations/${operationId}`),
};
