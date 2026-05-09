import type {
  AsyncOperationDetailResponse,
  OpsAsyncOperationStatus,
} from "./generated";

/**
 * Canonical async-operation polling payload returned by
 * `GET /v1/ops/operations/{id}`. Any batch endpoint (batch rerun, batch
 * delete, future batch annotate …) creates a parent `ops_async_operation`
 * row and spawns per-subject children linked via `parentAsyncOperationId`.
 * Pollers render `progress.completed / progress.total` and stop when
 * `status` transitions to `ready`, `failed`, `canceled`, or `superseded`.
 */
export type AsyncOperationStatus = OpsAsyncOperationStatus;
export type AsyncOperationDetail = AsyncOperationDetailResponse;

/** Terminal statuses — polling must stop on any of these. */
export const ASYNC_OPERATION_TERMINAL_STATES: ReadonlySet<AsyncOperationStatus> =
  new Set(["ready", "failed", "canceled", "superseded"]);
