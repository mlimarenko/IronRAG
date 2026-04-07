import { apiFetch } from "./client";

export const documentsApi = {
  list: (libraryId: string, params?: { search?: string; status?: string }) => {
    const qs = new URLSearchParams();
    qs.set("libraryId", libraryId);
    if (params?.search) qs.set("search", params.search);
    if (params?.status) qs.set("status", params.status);
    return apiFetch<any>(`/content/documents?${qs}`);
  },
  get: (documentId: string) => apiFetch<any>(`/content/documents/${documentId}`),
  upload: (libraryId: string, file: File, title?: string) => {
    const form = new FormData();
    form.append("library_id", libraryId);
    form.append("file", file);
    if (title) form.append("title", title);
    return fetch(`/v1/content/documents/upload`, {
      method: "POST",
      credentials: "include",
      body: form,
    }).then(r => r.json());
  },
  delete: (documentId: string) => apiFetch<void>(`/content/documents/${documentId}`, { method: "DELETE" }),
  reprocess: (documentId: string) => apiFetch<any>(`/content/documents/${documentId}/reprocess`, { method: "POST" }),
  createWebIngestRun: (data: { libraryId: string; seedUrl: string; mode: string; boundaryPolicy?: string; maxDepth?: number; maxPages?: number }) =>
    apiFetch<any>("/content/web-runs", {
      method: "POST",
      body: JSON.stringify(data),
    }),
  append: (documentId: string, text: string) =>
    apiFetch<any>(`/content/documents/${documentId}/append`, {
      method: "POST",
      body: JSON.stringify({ appendedText: text }),
    }),
  replace: (documentId: string, file: File) => {
    const form = new FormData();
    form.append("file", file);
    return fetch(`/v1/content/documents/${documentId}/replace`, {
      method: "POST",
      credentials: "include",
      body: form,
    }).then(r => {
      if (!r.ok) return r.json().then(b => { throw new Error(b?.error || `API error ${r.status}`); });
      return r.json();
    });
  },
  getHead: (documentId: string) => apiFetch<any>(`/content/documents/${documentId}/head`),
  getPreparedSegments: (documentId: string) => apiFetch<any>(`/content/documents/${documentId}/prepared-segments`),
  getTechnicalFacts: (documentId: string) => apiFetch<any>(`/content/documents/${documentId}/technical-facts`),
  getRevisions: (documentId: string) => apiFetch<any>(`/content/documents/${documentId}/revisions`),
  batchDelete: (documentIds: string[]) =>
    apiFetch<any>(`/content/documents/batch-delete`, {
      method: 'POST',
      body: JSON.stringify({ documentIds }),
    }),
  batchCancel: (documentIds: string[]) =>
    apiFetch<any>(`/content/documents/batch-cancel`, {
      method: 'POST',
      body: JSON.stringify({ documentIds }),
    }),
  batchReprocess: (documentIds: string[]) =>
    apiFetch<any>(`/content/documents/batch-reprocess`, {
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
