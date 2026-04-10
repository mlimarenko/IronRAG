import { apiFetch } from "./client";

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

export interface BatchReprocessResponse {
  reprocessedCount: number;
  failedCount: number;
  results: Array<BatchMutationErrorResult>;
}

/**
 * Raw document list/detail payload returned by `/v1/content/documents`.
 *
 * The shape is intentionally permissive: callers run it through
 * `mapApiDocument` to project into the canonical `DocumentItem` view model,
 * and a few code paths read additional snake_case aliases that the backend
 * still emits during normalization.
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

export interface RawPreparedSegmentItem {
  text?: string;
  content?: string;
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

export interface CreateWebIngestRunRequest {
  libraryId: string;
  seedUrl: string;
  mode: string;
  boundaryPolicy?: string;
  maxDepth?: number;
  maxPages?: number;
}

export const documentsApi = {
  list: (libraryId: string, params?: { search?: string; status?: string }) => {
    const qs = new URLSearchParams();
    qs.set("libraryId", libraryId);
    if (params?.search) qs.set("search", params.search);
    if (params?.status) qs.set("status", params.status);
    return apiFetch<RawDocumentResponse[]>(`/content/documents?${qs}`);
  },
  get: (documentId: string) =>
    apiFetch<RawDocumentResponse>(`/content/documents/${documentId}`),
  upload: (libraryId: string, file: File, title?: string): Promise<DocumentUploadResponse> => {
    const form = new FormData();
    form.append("library_id", libraryId);
    form.append("file", file);
    if (title) form.append("title", title);
    return apiFetch<DocumentUploadResponse>("/content/documents/upload", {
      method: "POST",
      body: form,
    });
  },
  delete: (documentId: string) =>
    apiFetch<void>(`/content/documents/${documentId}`, { method: "DELETE" }),
  reprocess: (documentId: string) =>
    apiFetch<DocumentReprocessResponse>(`/content/documents/${documentId}/reprocess`, {
      method: "POST",
      body: JSON.stringify({}),
    }),
  createWebIngestRun: (data: CreateWebIngestRunRequest) =>
    apiFetch<RawWebIngestRunResponse>("/content/web-runs", {
      method: "POST",
      body: JSON.stringify(data),
    }),
  append: (documentId: string, text: string) =>
    apiFetch<DocumentMutationResponse>(`/content/documents/${documentId}/append`, {
      method: "POST",
      body: JSON.stringify({ appendedText: text }),
    }),
  replace: (documentId: string, file: File): Promise<DocumentMutationResponse> => {
    const form = new FormData();
    form.append("file", file);
    return apiFetch<DocumentMutationResponse>(`/content/documents/${documentId}/replace`, {
      method: "POST",
      body: form,
    });
  },
  getHead: (documentId: string) =>
    apiFetch<RawDocumentResponse>(`/content/documents/${documentId}/head`),
  getPreparedSegments: (documentId: string) =>
    apiFetch<RawPreparedSegmentItem[]>(`/content/documents/${documentId}/prepared-segments`),
  getTechnicalFacts: (documentId: string) =>
    apiFetch<RawTechnicalFactItem[]>(`/content/documents/${documentId}/technical-facts`),
  getRevisions: (documentId: string) =>
    apiFetch<RawDocumentRevisionItem[]>(`/content/documents/${documentId}/revisions`),
  batchDelete: (documentIds: string[]) =>
    apiFetch<BatchDeleteResponse>(`/content/documents/batch-delete`, {
      method: 'POST',
      body: JSON.stringify({ documentIds }),
    }),
  batchCancel: (documentIds: string[]) =>
    apiFetch<BatchCancelResponse>(`/content/documents/batch-cancel`, {
      method: 'POST',
      body: JSON.stringify({ documentIds }),
    }),
  batchReprocess: (documentIds: string[]) =>
    apiFetch<BatchReprocessResponse>(`/content/documents/batch-reprocess`, {
      method: 'POST',
      body: JSON.stringify({ documentIds }),
    }),
};

export interface DocumentCostSummary {
  documentId: string;
  totalCost: string;
  currencyCode: string;
  providerCallCount: number;
}

export const billingApi = {
  getLibraryDocumentCosts: (libraryId: string) =>
    apiFetch<DocumentCostSummary[]>(`/billing/library-document-costs?libraryId=${libraryId}`),
};
