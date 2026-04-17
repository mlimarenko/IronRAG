import type { DocumentReadiness, DocumentStatus, SourceAccess } from "@/types";

import { ApiError, type ApiErrorBody, apiFetch } from "./client";

/**
 * Slim payload the documents list endpoint emits per row. Intentionally
 * small so a 5000-row library fits in a single 50-row keyset page under
 * ~30KB on the wire and the canonical status/readiness fields arrive
 * ready-to-render — no client-side derivation.
 */
export interface DocumentListItem {
  id: string;
  libraryId: string;
  workspaceId: string;
  fileName: string;
  fileType: string | null;
  fileSize: number | null;
  uploadedAt: string;
  documentState: string;
  status: DocumentStatus;
  readiness: DocumentReadiness;
  stage?: string;
  processingStartedAt?: string;
  processingFinishedAt?: string;
  retryable: boolean;
  sourceKind?: string;
  sourceUri?: string;
  sourceAccess?: SourceAccess;
}

export interface DocumentListPageResponse {
  items: DocumentListItem[];
  /** Opaque base64url token — pass back as `cursor` to fetch the next page. */
  nextCursor: string | null;
  /**
   * Total document count for the library when `includeTotal=true` was
   * requested. `null` when the client did not opt in (the count query is
   * expensive enough that we only pay for it on the first page).
   */
  totalCount: number | null;
  /**
   * Per-bucket document counts for the filter pills. Non-null whenever
   * `includeTotal=true` was requested — computed by the same aggregate
   * query that fills `totalCount`, so there's no separate round-trip.
   */
  statusCounts: DocumentListStatusCounts | null;
}

export interface DocumentListStatusCounts {
  total: number;
  ready: number;
  processing: number;
  queued: number;
  failed: number;
  canceled: number;
}

export type DocumentListSortKey =
  | "uploaded_at"
  | "file_name"
  | "file_type"
  | "file_size"
  | "status";
export type DocumentListSortOrder = "asc" | "desc";

/**
 * Canonical derived status buckets. Mirrors the backend `derived_status`
 * column in `list_document_page_rows`. The 5 values are the only ones
 * accepted by the `status` query parameter; anything else is rejected as
 * `400 Bad Request`.
 */
export type DocumentListStatusFilter =
  | "canceled"
  | "failed"
  | "processing"
  | "queued"
  | "ready";

export const DOCUMENT_LIST_STATUS_FILTERS: DocumentListStatusFilter[] = [
  "ready",
  "processing",
  "queued",
  "failed",
  "canceled",
];

export interface DocumentListParams {
  libraryId: string;
  cursor?: string;
  limit?: number;
  search?: string;
  sortBy?: DocumentListSortKey;
  sortOrder?: DocumentListSortOrder;
  includeDeleted?: boolean;
  includeTotal?: boolean;
  /** Empty / undefined = no filter. Sent as a comma-separated list. */
  status?: DocumentListStatusFilter[];
}

interface BatchMutationErrorResult {
  documentId: string;
  success: boolean;
  error: string | null;
}

export interface BatchDeleteResponse {
  deletedCount: number;
  failedCount: number;
  results: Array<BatchMutationErrorResult & { deleted: boolean }>;
}

export interface BatchCancelResponse {
  cancelledCount: number;
  failedCount: number;
  results: Array<BatchMutationErrorResult & { jobsCancelled: number }>;
}

/**
 * Canonical 202 Accepted payload for `POST /content/documents/batch-reprocess`.
 *
 * The server schedules the actual per-document reruns on a background task
 * and returns the id of a **parent** `ops_async_operation` that the client
 * polls via {@link opsApi.getAsyncOperation} to observe progress. All child
 * per-document mutations are linked back to this parent, so a single
 * indexed count query covers "completed / total / failed".
 */
export interface BatchReprocessAcceptedResponse {
  batchOperationId: string;
  total: number;
  libraryId: string;
  workspaceId: string;
}

/**
 * Raw detail payload returned by `/v1/content/documents/{id}`. The detail
 * endpoint is intentionally richer than the list endpoint and still emits a
 * mix of nested `readinessSummary` / `activeRevision` fields that the
 * documents inspector consumes directly.
 */
export interface RawDocumentResponse {
  id?: string;
  fileName?: string;
  readinessSummary?: {
    readinessKind?: string;
    activityStatus?: string;
    graphCoverageKind?: string;
    [key: string]: unknown;
  };
  activeRevision?: Record<string, unknown>;
  active_revision?: Record<string, unknown>;
  [key: string]: unknown;
}

export interface PreparedSegmentItem {
  segment?: {
    ordinal?: number;
    blockKind?: string;
    block_kind?: string;
    headingTrail?: string[];
    heading_trail?: string[];
    sectionPath?: string[];
    section_path?: string[];
    pageNumber?: number | null;
    page_number?: number | null;
  };
  text?: string;
  content?: string;
  normalizedText?: string;
  normalized_text?: string;
  parentBlockId?: string | null;
  parent_block_id?: string | null;
  tableCoordinates?: {
    rowIndex?: number;
    row_index?: number;
    columnIndex?: number;
    column_index?: number;
    rowSpan?: number;
    row_span?: number;
    columnSpan?: number;
    column_span?: number;
  } | null;
  table_coordinates?: {
    row_index?: number;
    column_index?: number;
    row_span?: number;
    column_span?: number;
  } | null;
  codeLanguage?: string | null;
  code_language?: string | null;
  [key: string]: unknown;
}

export interface RawTechnicalFactItem {
  [key: string]: unknown;
}

export interface RawDocumentRevisionItem {
  [key: string]: unknown;
}

export interface RawWebIngestRunResponse {
  id?: string;
  state?: string;
  [key: string]: unknown;
}

interface RawWebIngestRunListItem {
  runId?: string;
  seedUrl?: string;
  runState?: string;
  mode?: string;
  boundaryPolicy?: string;
  maxDepth?: number;
  maxPages?: number;
  counts?: {
    discovered?: number;
    processed?: number;
  };
}

interface RawWebIngestRunPage {
  normalizedUrl?: string;
  discoveredUrl?: string;
  candidateState?: string;
}

interface ListEnvelope<T> {
  items?: T[];
}

export interface WebIngestRunListItem {
  runId: string;
  seedUrl: string;
  runState: string;
  mode: string;
  boundaryPolicy?: string;
  maxDepth?: number;
  maxPages?: number;
  counts?: {
    discovered?: number;
    processed?: number;
  };
}

export interface WebIngestRunPageItem {
  normalizedUrl?: string;
  discoveredUrl?: string;
  candidateState?: string;
}

export interface PreparedSegmentsPageResponse {
  items?: PreparedSegmentItem[];
  [key: string]: unknown;
}

export interface TechnicalFactsPageResponse {
  items?: RawTechnicalFactItem[];
  [key: string]: unknown;
}

export interface DocumentUploadResponse {
  documentId?: string;
  [key: string]: unknown;
}

export interface DocumentReprocessResponse {
  documentId?: string;
  [key: string]: unknown;
}

export interface DocumentMutationResponse {
  documentId?: string;
  [key: string]: unknown;
}

export interface DocumentUploadOptions {
  externalKey?: string;
  fileName?: string;
  title?: string;
}

function normalizeListItems<T>(raw: T[] | ListEnvelope<T>): T[] {
  return Array.isArray(raw) ? raw : (raw.items ?? []);
}

function mapWebIngestRunListItem(
  raw: RawWebIngestRunListItem,
): WebIngestRunListItem {
  return {
    runId: raw.runId ?? "",
    seedUrl: raw.seedUrl ?? "",
    runState: raw.runState ?? "accepted",
    mode: raw.mode ?? "single_page",
    boundaryPolicy: raw.boundaryPolicy,
    maxDepth: raw.maxDepth,
    maxPages: raw.maxPages,
    counts: raw.counts,
  };
}

function mapWebIngestRunPageItem(
  raw: RawWebIngestRunPage,
): WebIngestRunPageItem {
  return {
    normalizedUrl: raw.normalizedUrl,
    discoveredUrl: raw.discoveredUrl,
    candidateState: raw.candidateState,
  };
}

export interface CreateWebIngestRunRequest {
  libraryId: string;
  seedUrl: string;
  mode: string;
  boundaryPolicy?: string;
  maxDepth?: number;
  maxPages?: number;
}

export const documentsApi = {
  /**
   * Canonical keyset-paginated list. Callers drive infinite scroll by
   * threading `nextCursor` back into the next call; there is no
   * array-returning legacy shape. `includeTotal` is opt-in because the
   * backend executes a second unbounded `COUNT(*)` when it is set and
   * should only be requested once per library open.
   */
  list: (params: DocumentListParams): Promise<DocumentListPageResponse> => {
    const qs = new URLSearchParams();
    qs.set("libraryId", params.libraryId);
    if (params.cursor) qs.set("cursor", params.cursor);
    if (params.limit != null) qs.set("limit", String(params.limit));
    if (params.search) qs.set("search", params.search);
    if (params.sortBy) qs.set("sortBy", params.sortBy);
    if (params.sortOrder) qs.set("sortOrder", params.sortOrder);
    if (params.includeDeleted) qs.set("includeDeleted", "true");
    if (params.includeTotal) qs.set("includeTotal", "true");
    if (params.status && params.status.length > 0) {
      qs.set("status", params.status.join(","));
    }
    return apiFetch<DocumentListPageResponse>(`/content/documents?${qs}`);
  },
  get: (documentId: string) =>
    apiFetch<RawDocumentResponse>(`/content/documents/${documentId}`),
  upload: (
    libraryId: string,
    file: File,
    options?: DocumentUploadOptions,
  ): Promise<DocumentUploadResponse> => {
    const form = new FormData();
    form.append("library_id", libraryId);
    form.append("file", file, options?.fileName ?? file.name);
    if (options?.externalKey) form.append("external_key", options.externalKey);
    if (options?.title) form.append("title", options.title);
    return apiFetch<DocumentUploadResponse>("/content/documents/upload", {
      method: "POST",
      body: form,
    });
  },
  delete: (documentId: string) =>
    apiFetch<void>(`/content/documents/${documentId}`, { method: "DELETE" }),
  reprocess: (documentId: string) =>
    apiFetch<DocumentReprocessResponse>(
      `/content/documents/${documentId}/reprocess`,
      {
        method: "POST",
        body: JSON.stringify({}),
      },
    ),
  createWebIngestRun: (data: CreateWebIngestRunRequest) =>
    apiFetch<RawWebIngestRunResponse>("/content/web-runs", {
      method: "POST",
      body: JSON.stringify(data),
    }),
  listWebRuns: async (libraryId: string): Promise<WebIngestRunListItem[]> => {
    const response = await apiFetch<
      RawWebIngestRunListItem[] | ListEnvelope<RawWebIngestRunListItem>
    >(`/content/web-runs?libraryId=${libraryId}`);
    return normalizeListItems(response).map(mapWebIngestRunListItem);
  },
  listWebRunPages: async (runId: string): Promise<WebIngestRunPageItem[]> => {
    const response = await apiFetch<
      RawWebIngestRunPage[] | ListEnvelope<RawWebIngestRunPage>
    >(`/content/web-runs/${runId}/pages`);
    return normalizeListItems(response).map(mapWebIngestRunPageItem);
  },
  edit: (documentId: string, markdown: string) =>
    apiFetch<DocumentMutationResponse>(
      `/content/documents/${documentId}/edit`,
      {
        method: "POST",
        body: JSON.stringify({ markdown }),
      },
    ),
  replace: (
    documentId: string,
    file: File,
  ): Promise<DocumentMutationResponse> => {
    const form = new FormData();
    form.append("file", file);
    return apiFetch<DocumentMutationResponse>(
      `/content/documents/${documentId}/replace`,
      {
        method: "POST",
        body: form,
      },
    );
  },
  getHead: (documentId: string) =>
    apiFetch<RawDocumentResponse>(`/content/documents/${documentId}/head`),
  getPreparedSegments: async (documentId: string) => {
    const response = await apiFetch<PreparedSegmentsPageResponse>(
      `/content/documents/${documentId}/prepared-segments`,
    );
    return response.items ?? [];
  },
  getSourceText: async (sourceHref: string) => {
    const response = await fetch(sourceHref, { credentials: "include" });
    if (!response.ok) {
      const body = (await response.json().catch(() => ({}))) as ApiErrorBody;
      throw new ApiError(response.status, body);
    }
    return response.text();
  },
  getTechnicalFacts: async (documentId: string) => {
    const response = await apiFetch<TechnicalFactsPageResponse>(
      `/content/documents/${documentId}/technical-facts`,
    );
    return response.items ?? [];
  },
  getRevisions: (documentId: string) =>
    apiFetch<RawDocumentRevisionItem[]>(
      `/content/documents/${documentId}/revisions`,
    ),
  batchDelete: (documentIds: string[]) =>
    apiFetch<BatchDeleteResponse>(`/content/documents/batch-delete`, {
      method: "POST",
      body: JSON.stringify({ documentIds }),
    }),
  batchCancel: (documentIds: string[]) =>
    apiFetch<BatchCancelResponse>(`/content/documents/batch-cancel`, {
      method: "POST",
      body: JSON.stringify({ documentIds }),
    }),
  batchReprocess: (documentIds: string[]) =>
    apiFetch<BatchReprocessAcceptedResponse>(
      `/content/documents/batch-reprocess`,
      {
        method: "POST",
        body: JSON.stringify({ documentIds }),
      },
    ),
};

export interface DocumentCostSummary {
  documentId: string;
  totalCost: string;
  currencyCode: string;
  providerCallCount: number;
}

export interface LibraryCostSummary {
  totalCost: string;
  currencyCode: string;
  documentCount: number;
  providerCallCount: number;
}

export const billingApi = {
  getLibraryDocumentCosts: (libraryId: string) =>
    apiFetch<DocumentCostSummary[]>(
      `/billing/library-document-costs?libraryId=${libraryId}`,
    ),
  getLibraryCostSummary: (libraryId: string) =>
    apiFetch<LibraryCostSummary>(
      `/billing/library-cost-summary?libraryId=${libraryId}`,
    ),
};

export type LibrarySnapshotIncludeKind = "library_data" | "blobs";

export type LibrarySnapshotOverwriteMode = "reject" | "replace";

export interface LibrarySnapshotImportReport {
  libraryId: string;
  overwriteMode: LibrarySnapshotOverwriteMode;
  includeKinds: LibrarySnapshotIncludeKind[];
  postgresRowsByTable: Record<string, number>;
  arangoDocsByCollection: Record<string, number>;
  arangoEdgesByCollection: Record<string, number>;
  blobsRestored: number;
}

/**
 * Canonical backup/restore API — tar.zst archive with optional family
 * selection. Export is triggered via plain navigation (no fetch, no Blob
 * buffering) so the browser streams the response straight to disk —
 * multi-GB libraries download cleanly without memory pressure. Import
 * uses `fetch` with a streaming body source (File); the body is the raw
 * archive, not multipart.
 */
export const librarySnapshotApi = {
  /**
   * Builds the canonical export URL. Navigate to this URL (via
   * `<a href download>` or `window.location`) to trigger a browser
   * download directly from the response body — no `fetch` wrapper.
   */
  exportUrl: (
    libraryId: string,
    include: LibrarySnapshotIncludeKind[],
  ): string => {
    const qs = new URLSearchParams();
    if (include.length > 0) qs.set("include", include.join(","));
    const query = qs.toString();
    const suffix = query ? `?${query}` : "";
    return `/v1/content/libraries/${libraryId}/snapshot${suffix}`;
  },
  /**
   * Triggers a browser download of the export URL. Creates an anchor
   * element, clicks it, and removes it — the browser handles the
   * streaming. No JavaScript memory buffer is allocated for the
   * archive body.
   */
  downloadExport: (
    libraryId: string,
    include: LibrarySnapshotIncludeKind[],
  ): void => {
    const url = librarySnapshotApi.exportUrl(libraryId, include);
    const anchor = document.createElement("a");
    anchor.href = url;
    anchor.rel = "noopener";
    document.body.appendChild(anchor);
    anchor.click();
    anchor.remove();
  },
  /**
   * Restores a library from a tar.zst archive. The include kinds are
   * read from the manifest inside the archive; the client only picks
   * the overwrite mode.
   */
  import: (
    libraryId: string,
    file: File,
    overwrite: LibrarySnapshotOverwriteMode,
  ): Promise<LibrarySnapshotImportReport> => {
    const qs = new URLSearchParams();
    if (overwrite !== "reject") qs.set("overwrite", overwrite);
    const query = qs.toString();
    const suffix = query ? `?${query}` : "";
    return apiFetch<LibrarySnapshotImportReport>(
      `/content/libraries/${libraryId}/snapshot${suffix}`,
      {
        method: "POST",
        headers: { "Content-Type": "application/zstd" },
        body: file,
      },
    );
  },
};
