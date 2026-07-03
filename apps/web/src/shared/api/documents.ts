import { Content, Ops } from "./generated";
import { client as generatedClient } from "./generated/client.gen";
import type {
  AsyncOperationDetailResponse,
  BatchCancelResponse,
  BatchDocumentOperationAcceptedResponse,
  ContentDocumentDetailResponse,
  ContentDocumentHead,
  ContentDocumentListItem,
  ContentMutationDetailResponse,
  ContentRevision,
  CreateDocumentResponse,
  CreateWebIngestRunRequest,
  DocumentListPageResponse as GeneratedDocumentListPageResponse,
  DocumentListSortKey as GeneratedDocumentListSortKey,
  DocumentListSortOrder as GeneratedDocumentListSortOrder,
  DocumentStatus as GeneratedDocumentStatus,
  ListContentDocumentsData,
  ListContentPreparedSegmentsData,
  OverwriteMode,
  PreparedSegmentDetail,
  PreparedSegmentsPageResponse,
  SnapshotImportAcceptedResponse,
  TechnicalFactsPageResponse,
  TypedTechnicalFact,
  WebDiscoveredPage,
  WebIngestRunReceipt as GeneratedWebIngestRunReceipt,
  WebIngestRunSummary,
  WorkspaceLibraryImportReportResponse,
} from "./generated";
import { ASYNC_OPERATION_TERMINAL_STATES } from "./ops";
import { ApiError, type ApiErrorBody, unwrap } from "./runtime";

export type DocumentListItem = ContentDocumentListItem;
export type DocumentListPageResponse = GeneratedDocumentListPageResponse;
export type DocumentListSortKey = GeneratedDocumentListSortKey;
export type DocumentListSortOrder = GeneratedDocumentListSortOrder;

/**
 * Canonical derived status buckets. Mirrors the backend `derived_status`
 * column in `list_document_page_rows`. The 5 values are the only ones
 * accepted by the `status` query parameter; anything else is rejected as
 * `400 Bad Request`.
 */
export type DocumentListStatusFilter =
  GeneratedDocumentStatus;

export const DOCUMENT_LIST_STATUS_FILTERS: DocumentListStatusFilter[] = [
  "ready",
  "processing",
  "queued",
  "failed",
  "canceled",
];

interface DocumentListParams {
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
  /**
   * Restrict the page to specific document ids. Empty / undefined = no
   * filter. Sent as a comma-separated list. Used to resolve a deep-linked
   * `documentId` through the canonical list derivation so the inspector
   * opens even when the target is off the loaded page.
   */
  ids?: string[];
}

type BatchDeleteResponse = BatchDocumentOperationAcceptedResponse;

/**
 * Canonical 202 Accepted payload for `POST /content/documents/batch-reprocess`.
 *
 * The server schedules the actual per-document reruns on a background task
 * and returns the id of a **parent** `ops_async_operation` that the client
 * polls through the generated async-operation query to observe progress. All child
 * per-document mutations are linked back to this parent, so a single
 * indexed count query covers "completed / total / failed".
 */
type BatchReprocessAcceptedResponse =
  BatchDocumentOperationAcceptedResponse;

export type PreparedSegmentItem = PreparedSegmentDetail;
export type WebIngestRunListItem = WebIngestRunSummary;
export type WebIngestRunPageItem = WebDiscoveredPage;
type WebIngestRunReceipt = GeneratedWebIngestRunReceipt;

type DocumentUploadResponse = CreateDocumentResponse;
type DocumentReprocessResponse = ContentMutationDetailResponse;
type DocumentMutationResponse = ContentMutationDetailResponse;

type UpdateDocumentHintRequest = {
  documentHint: string | null;
};

type UpdateDocumentHintResponse = {
  activeRevision?: {
    documentHint?: string | null;
  } | null;
};

interface DocumentUploadOptions {
  documentHint?: string | undefined;
  externalKey?: string | undefined;
  fileName?: string | undefined;
  title?: string | undefined;
}

interface PreparedSegmentsPageParams {
  offset?: number;
  limit?: number;
}

const PREPARED_SEGMENTS_PAGE_LIMIT = 500;

export const documentsApi = {
  /**
   * Canonical keyset-paginated list. Callers drive infinite scroll by
   * threading `nextCursor` back into the next call; there is no
   * array-only shape. `includeTotal` is opt-in because the
   * backend executes a second unbounded `COUNT(*)` when it is set and
   * should only be requested once per library open.
  */
  list: (params: DocumentListParams): Promise<DocumentListPageResponse> => {
    const query: NonNullable<ListContentDocumentsData["query"]> = {
      libraryId: params.libraryId,
    };
    if (params.cursor !== undefined) query.cursor = params.cursor;
    if (params.limit !== undefined) query.limit = params.limit;
    if (params.search !== undefined) query.search = params.search;
    if (params.sortBy !== undefined) query.sortBy = params.sortBy;
    if (params.sortOrder !== undefined) query.sortOrder = params.sortOrder;
    if (params.includeDeleted !== undefined) query.includeDeleted = params.includeDeleted;
    if (params.includeTotal !== undefined) query.includeTotal = params.includeTotal;
    if (params.status && params.status.length > 0) query.status = params.status.join(",");
    if (params.ids && params.ids.length > 0) query.ids = params.ids.join(",");

    return Content.listContentDocuments({ query }).then(
      (result): DocumentListPageResponse => unwrap(result),
    );
  },
  get: (documentId: string) =>
    Content.getContentDocument({ path: { documentId } }).then(
      (result): ContentDocumentDetailResponse => unwrap(result),
    ),
  upload: (
    libraryId: string,
    file: File,
    options?: DocumentUploadOptions,
  ): Promise<DocumentUploadResponse> => {
    // The generated SDK uses `formDataBodySerializer`, which constructs a
    // FormData from `Object.entries(body)`. Passing a pre-built FormData
    // here would result in an empty multipart payload (Object.entries on a
    // FormData yields nothing), so the backend received zero fields and
    // rejected the upload with `bad request: missing library_id`. Pass a
    // plain object and let the serializer build the multipart form.
    const fileBlob =
      options?.fileName && options.fileName !== file.name
        ? new File([file], options.fileName, { type: file.type })
        : file;
    const body: Record<string, unknown> = {
      library_id: libraryId,
      file: fileBlob,
    };
    if (options?.documentHint) body.document_hint = options.documentHint;
    if (options?.externalKey) body.external_key = options.externalKey;
    if (options?.title) body.title = options.title;
    return Content.uploadContentDocument({ body }).then(
      (result): DocumentUploadResponse => unwrap(result),
    );
  },
  delete: (documentId: string) =>
    Content.deleteContentDocument({ path: { documentId } }).then((result) => {
      unwrap(result);
    }),
  updateDocumentHint: async (
    documentId: string,
    documentHint: string | null,
  ): Promise<string | null> => {
    const body: UpdateDocumentHintRequest = { documentHint };
    const response = await fetch(`/v1/content/documents/${encodeURIComponent(documentId)}`, {
      body: JSON.stringify(body),
      credentials: "include",
      headers: { "Content-Type": "application/json" },
      method: "PATCH",
    });
    if (!response.ok) {
      throw new ApiError(response.status, await readApiErrorBody(response));
    }
    if (response.status === 204) {
      return documentHint;
    }

    const text = await response.text();
    if (!text) {
      return documentHint;
    }
    const payload = JSON.parse(text) as UpdateDocumentHintResponse;
    const savedDocumentHint = payload.activeRevision?.documentHint;
    return typeof savedDocumentHint === "string" || savedDocumentHint === null
      ? savedDocumentHint
      : documentHint;
  },
  reprocess: (documentId: string) =>
    Content.reprocessContentDocument({
      path: { documentId },
      body: {},
    }).then((result): DocumentReprocessResponse => unwrap(result)),
  createWebIngestRun: (data: CreateWebIngestRunRequest) =>
    Content.createContentWebIngestRun({ body: data }).then(
      (result): WebIngestRunReceipt => unwrap(result),
    ),
  listWebRuns: async (
    libraryId: string,
    limit: number = 50,
  ): Promise<WebIngestRunListItem[]> => {
    return Content.listContentWebIngestRuns({
      query: { libraryId, limit },
    }).then((result): WebIngestRunListItem[] => unwrap(result));
  },
  listWebRunPages: async (runId: string): Promise<WebIngestRunPageItem[]> => {
    return Content.listContentWebIngestRunPages({
      path: { runId },
    }).then((result): WebIngestRunPageItem[] => unwrap(result));
  },
  cancelWebRun: (runId: string) =>
    Content.cancelContentWebIngestRun({ path: { runId } }).then(
      (result): WebIngestRunReceipt => unwrap(result),
    ),
  edit: (documentId: string, markdown: string) =>
    Content.editContentDocument({
      path: { documentId },
      body: { markdown },
    }).then((result): DocumentMutationResponse => unwrap(result)),
  replace: (
    documentId: string,
    file: File,
  ): Promise<DocumentMutationResponse> => {
    // Same FormData/serializer constraint as `upload` above — the
    // generated client constructs the multipart body from a plain
    // object, not a pre-built FormData.
    return Content.replaceContentDocument({
      path: { documentId },
      body: { file },
    }).then((result): DocumentMutationResponse => unwrap(result));
  },
  getHead: (documentId: string) =>
    Content.getContentDocumentHead({ path: { documentId } }).then(
      (result): ContentDocumentHead => unwrap(result),
    ),
  getPreparedSegmentsPage: (
    documentId: string,
    params: PreparedSegmentsPageParams = {},
  ): Promise<PreparedSegmentsPageResponse> => {
    const query: NonNullable<ListContentPreparedSegmentsData["query"]> = {};
    if (params.offset !== undefined) query.offset = params.offset;
    if (params.limit !== undefined) query.limit = params.limit;

    return Content.listContentPreparedSegments({
      path: { documentId },
      query,
    }).then((result): PreparedSegmentsPageResponse => unwrap(result));
  },
  getAllPreparedSegments: async (documentId: string) => {
    const segments: PreparedSegmentItem[] = [];
    let offset = 0;

    while (true) {
      const response = await documentsApi.getPreparedSegmentsPage(documentId, {
        offset,
        limit: PREPARED_SEGMENTS_PAGE_LIMIT,
      });
      const pageItems = response.items ?? [];
      segments.push(...pageItems);

      const total = typeof response.total === "number" ? response.total : null;
      if (pageItems.length === 0) {
        break;
      }
      if (total != null && segments.length >= total) {
        break;
      }
      if (total == null && pageItems.length < PREPARED_SEGMENTS_PAGE_LIMIT) {
        break;
      }

      offset += pageItems.length;
    }

    return segments;
  },
  getTechnicalFacts: async (documentId: string): Promise<TypedTechnicalFact[]> => {
    const response = await Content.listContentTechnicalFacts({
      path: { documentId },
    }).then((result): TechnicalFactsPageResponse => unwrap(result));
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
  getEditorSourceText: async (documentId: string) => {
    const query = new URLSearchParams({ representation: "editor_markdown" });
    const response = await fetch(
      `/v1/content/documents/${encodeURIComponent(documentId)}/source?${query}`,
      { credentials: "include" },
    );
    if (!response.ok) {
      throw new ApiError(response.status, await readApiErrorBody(response));
    }
    return response.text();
  },
  getRevisions: (documentId: string) =>
    Content.listContentRevisions({ path: { documentId } }).then(
      (result): ContentRevision[] => unwrap(result),
    ),
  batchDelete: (documentIds: string[]) =>
    Content.batchDeleteContentDocuments({
      body: { documentIds },
    }).then((result): BatchDeleteResponse => unwrap(result)),
  batchCancel: (documentIds: string[]) =>
    Content.batchCancelContentDocuments({
      body: { documentIds },
    }).then((result): BatchCancelResponse => unwrap(result)),
  batchReprocess: (documentIds: string[]) =>
    Content.batchReprocessContentDocuments({
      body: { documentIds },
    }).then((result): BatchReprocessAcceptedResponse => unwrap(result)),
};

export type LibrarySnapshotIncludeKind =
  | "library_data"
  | "blobs"
  | "workspace"
  | "ai_config";

export type LibrarySnapshotOverwriteMode = OverwriteMode;

type LibrarySnapshotImportReport = WorkspaceLibraryImportReportResponse;
type LibrarySnapshotImportAccepted = SnapshotImportAcceptedResponse;

export type LibrarySnapshotImportResult =
  | { kind: "completed"; report: LibrarySnapshotImportReport }
  | { kind: "accepted"; operation: LibrarySnapshotImportAccepted };

const SNAPSHOT_IMPORT_POLL_INTERVAL_MS = 2_000;
const SNAPSHOT_IMPORT_TIMEOUT_MS = 30 * 60 * 1_000;

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function snapshotApiBaseUrl(): string {
  const configured = generatedClient.getConfig().baseUrl;
  if (configured) return configured.replace(/\/$/, "");
  if (typeof globalThis.location !== "undefined" && globalThis.location.origin !== "null") {
    return globalThis.location.origin;
  }
  return "";
}

async function readApiErrorBody(response: Response): Promise<ApiErrorBody> {
  const text = await response.text();
  if (!text) return { error: `API error ${response.status}` };
  try {
    const parsed = JSON.parse(text) as unknown;
    if (parsed && typeof parsed === "object") {
      return parsed as ApiErrorBody;
    }
  } catch {
    // Fall through to a plain-text error body.
  }
  return { error: text };
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
  ): Promise<LibrarySnapshotImportResult> => {
    const query = overwrite !== "reject"
      ? `?overwrite=${encodeURIComponent(overwrite)}`
      : "";
    return fetch(
      `${snapshotApiBaseUrl()}/v1/content/libraries/${encodeURIComponent(libraryId)}/snapshot${query}`,
      {
        body: file,
        credentials: "include",
        headers: { "Content-Type": "application/zstd" },
        method: "POST",
      },
    ).then(async (response): Promise<LibrarySnapshotImportResult> => {
      if (!response.ok) {
        throw new ApiError(response.status, await readApiErrorBody(response));
      }
      const payload = await response.json();
      if (response.status === 202 && typeof payload?.operationId === "string") {
        return {
          kind: "accepted",
          operation: payload as LibrarySnapshotImportAccepted,
        };
      }
      return {
        kind: "completed",
        report: payload as LibrarySnapshotImportReport,
      };
    });
  },
  waitForImport: async (
    operationId: string,
    timeoutMs = SNAPSHOT_IMPORT_TIMEOUT_MS,
  ): Promise<AsyncOperationDetailResponse> => {
    const deadline = Date.now() + timeoutMs;
    while (true) {
      const operation = await Ops.getAsyncOperation({
        path: { operationId },
      }).then((result): AsyncOperationDetailResponse => unwrap(result));
      if (ASYNC_OPERATION_TERMINAL_STATES.has(operation.status)) {
        return operation;
      }
      if (Date.now() >= deadline) {
        throw new Error("snapshot_import_timeout");
      }
      await sleep(SNAPSHOT_IMPORT_POLL_INTERVAL_MS);
    }
  },
};
