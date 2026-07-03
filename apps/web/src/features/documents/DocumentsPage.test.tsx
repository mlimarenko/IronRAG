import { act } from 'react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { createRoot, type Root } from 'react-dom/client';
import { MemoryRouter, useLocation } from 'react-router-dom';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import DocumentsPage from '@/features/documents/DocumentsPage';
import { TooltipProvider } from '@/shared/components/ui/tooltip';
import { getTableStateStorageKey } from '@/shared/hooks/useTableState';

const {
  useAppMock,
  documentsApiMock,
  adminApiMock,
  libraryCostSummaryFn,
} = vi.hoisted(() => ({
  useAppMock: vi.fn(),
  libraryCostSummaryFn: vi.fn(),
  documentsApiMock: {
    list: vi.fn(),
    get: vi.fn(),
    getSourceText: vi.fn(),
    getEditorSourceText: vi.fn(),
    upload: vi.fn(),
    delete: vi.fn(),
    reprocess: vi.fn(),
    edit: vi.fn(),
    replace: vi.fn(),
    getPreparedSegmentsPage: vi.fn(),
    getAllPreparedSegments: vi.fn(),
    getTechnicalFacts: vi.fn(),
    listWebRuns: vi.fn(),
    listWebRunPages: vi.fn(),
  },
  adminApiMock: {
    getLibrary: vi.fn(),
    updateWebIngestPolicy: vi.fn(),
  },
}));

vi.mock('@/shared/contexts/app-context', () => ({
  useApp: () => useAppMock(),
}));

vi.mock('@/shared/api', () => ({
  documentsApi: documentsApiMock,
  adminApi: adminApiMock,
  DOCUMENT_LIST_STATUS_FILTERS: [],
  ASYNC_OPERATION_TERMINAL_STATES: new Set(['ready', 'failed', 'canceled', 'superseded']),
  // TanStack Query option stubs — each returns a plain queryOptions shape
  // whose queryFn delegates to the existing *Mock fns so historical
  // assertions keep working without rebuilding tests around the SDK classes.
  queries: {
    listContentDocumentsOptions: (
      input?: { query?: { libraryId?: string; limit?: number; cursor?: string; search?: string; sortBy?: string; sortOrder?: string; includeTotal?: boolean; status?: string[]; ids?: string } },
    ) => ({
      queryKey: ['mockedListContentDocuments', input?.query ?? null],
      queryFn: async () => {
        const q = input?.query ?? {};
        return documentsApiMock.list({
          libraryId: q.libraryId,
          limit: q.limit,
          cursor: q.cursor,
          search: q.search,
          sortBy: q.sortBy,
          sortOrder: q.sortOrder,
          includeTotal: q.includeTotal,
          status: q.status,
          ids: q.ids,
        });
      },
    }),
    getLibraryCostSummaryOptions: (
      input: { query: { libraryId: string } },
    ) => ({
      queryKey: ['mockedLibraryCostSummary', input.query.libraryId],
      queryFn: async () => libraryCostSummaryFn(input.query.libraryId),
    }),
    getWorkspaceCostSummaryOptions: (
      input: { query: { workspaceId: string } },
    ) => ({
      queryKey: ['mockedWorkspaceCostSummary', input.query.workspaceId],
      queryFn: async () => ({ totalCost: '0', currencyCode: 'USD', libraryCount: 0, documentCount: 0, providerCallCount: 0 }),
    }),
    getCatalogLibraryOptions: (
      input: { path: { libraryId: string } },
    ) => ({
      queryKey: ['mockedCatalogLibrary', input.path.libraryId],
      queryFn: async () => adminApiMock.getLibrary(input.path.libraryId),
    }),
    getContentDocumentOptions: (
      input: { path: { documentId: string } },
    ) => ({
      queryKey: ['mockedContentDocument', input.path.documentId],
      queryFn: async () => documentsApiMock.get(input.path.documentId),
    }),
    listContentPreparedSegmentsOptions: (
      input: { path: { documentId: string }; query?: { limit?: number } },
    ) => ({
      queryKey: ['mockedPreparedSegments', input.path.documentId],
      queryFn: async () =>
        documentsApiMock.getPreparedSegmentsPage(input.path.documentId, { limit: input.query?.limit }),
    }),
    listContentTechnicalFactsOptions: (
      input: { path: { documentId: string } },
    ) => ({
      queryKey: ['mockedTechnicalFacts', input.path.documentId],
      queryFn: async () => documentsApiMock.getTechnicalFacts(input.path.documentId),
    }),
    getAsyncOperationOptions: (
      input: { path: { operationId: string } },
    ) => ({
      queryKey: ['mockedAsyncOperation', input.path.operationId],
      queryFn: async () => ({ status: 'ready', progress: { total: 0, completed: 0, failed: 0, inFlight: 0 } }),
    }),
  },
}));

vi.mock('@/features/documents/components/DocumentsPageHeader', () => ({
  DocumentsPageHeader: () => null,
}));

vi.mock('@/features/documents/components/DocumentsInspectorPanel', () => ({
  DocumentsInspectorPanel: (props: {
    editorActionReadOnly?: boolean;
    selectedDoc?: { fileName?: string } | null;
    onOpenEditor: () => void;
    onViewInGraph?: () => void;
  }) =>
    props.selectedDoc ? (
      <div>
        <button onClick={() => props.onOpenEditor()}>
          {props.editorActionReadOnly ? 'View' : 'Edit'} {props.selectedDoc.fileName}
        </button>
        {props.onViewInGraph && (
          <button onClick={() => props.onViewInGraph?.()}>View in Graph</button>
        )}
      </div>
    ) : null,
}));

vi.mock('@/features/documents/components/editor/DocumentEditorShell', () => ({
  DocumentEditorShell: (props: {
    open: boolean;
    documentName: string;
    onSave: (markdown: string) => void;
  }) =>
    props.open ? (
      <div data-testid="document-editor-shell">
        <span>{props.documentName}</span>
        <button onClick={() => props.onSave('## Sheet1\n\n| Item | Qty |\n| --- | --- |\n| Widget | 9 |')}>
          Save Editor
        </button>
      </div>
    ) : null,
}));

/**
 * Build a `DocumentListPageResponse`-shaped payload. The real backend emits a
 * rich object per row; these fixtures include every field the page actually
 * reads so we never test stubs that silently drop attributes.
 */
function listPage(
  items: Array<{
    id: string;
    fileName: string;
    fileType?: string;
    status?: 'ready' | 'processing' | 'queued' | 'failed' | 'canceled';
    readiness?: 'processing' | 'readable' | 'graph_sparse' | 'graph_ready' | 'failed';
    sourceKind?: string;
    sourceUri?: string;
    sourceAccess?: { kind: 'stored_document' | 'external_url'; href: string };
    cost?: string;
    progressPercent?: number;
    failureCode?: string;
    failureMessage?: string;
  }>,
) {
  return {
    items: items.map((raw) => ({
      id: raw.id,
      libraryId: 'library-1',
      workspaceId: 'ws-1',
      fileName: raw.fileName,
      fileType: raw.fileType ?? 'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet',
      fileSize: 2048,
      uploadedAt: '2026-04-10T12:00:00Z',
      documentState: 'active',
      status: raw.status ?? 'ready',
      readiness: raw.readiness ?? 'graph_ready',
      stage: 'finalizing',
      progressPercent: raw.progressPercent,
      failureCode: raw.failureCode,
      failureMessage: raw.failureMessage,
      retryable: false,
      sourceKind: raw.sourceKind,
      sourceUri: raw.sourceUri,
      sourceAccess: raw.sourceAccess,
      // Canonical per-row cost (0.3.2): emitted by the backend LATERAL
      // on `billing_execution_cost` via `list_document_page_rows`.
      // A raw `"0"` renders as `$0.000`; a missing / non-numeric value
      // renders as `—`.
      cost: raw.cost ?? '0',
      costCurrencyCode: 'USD',
    })),
    nextCursor: null,
    totalCount: items.length,
  };
}

function LocationProbe() {
  const location = useLocation();

  return <div data-testid="current-location">{`${location.pathname}${location.search}`}</div>;
}

describe('DocumentsPage', () => {
  let container: HTMLDivElement;
  let root: Root | null;

  beforeEach(() => {
    vi.clearAllMocks();
    localStorage.clear();
    container = document.createElement('div');
    document.body.appendChild(container);
    root = null;

    useAppMock.mockReturnValue({
      activeWorkspace: { id: 'ws-1', name: 'Workspace' },
      activeLibrary: { id: 'library-1', name: 'Docs' },
      locale: 'en',
    });

    documentsApiMock.list.mockResolvedValue(
      listPage([{ id: 'doc-1', fileName: 'inventory.xlsx', sourceKind: 'upload' }]),
    );
    documentsApiMock.get.mockResolvedValue({ id: 'doc-1', lifecycle: null });
    documentsApiMock.getPreparedSegmentsPage.mockResolvedValue({
      total: 2,
      offset: 0,
      limit: 1,
      items: [],
    });
    documentsApiMock.getAllPreparedSegments.mockResolvedValue([
      {
        segment: { ordinal: 0, blockKind: 'heading', headingTrail: ['Sheet1'] },
        text: '## Sheet1',
      },
      {
        segment: { ordinal: 1, blockKind: 'table' },
        text: '| Item | Qty |\n| --- | --- |\n| Widget | 7 |',
      },
    ]);
    documentsApiMock.getTechnicalFacts.mockResolvedValue([]);
    documentsApiMock.getSourceText.mockResolvedValue('def run():\n\treturn 42\n');
    documentsApiMock.getEditorSourceText.mockResolvedValue('## Sheet1\n\n| Item | Qty |\n| --- | --- |\n| Widget | 7 |');
    documentsApiMock.edit.mockResolvedValue({ documentId: 'doc-1' });
    documentsApiMock.listWebRuns.mockResolvedValue([]);
    documentsApiMock.listWebRunPages.mockResolvedValue([]);
    libraryCostSummaryFn.mockResolvedValue({
      totalCost: '0',
      currencyCode: 'USD',
      documentCount: 0,
      providerCallCount: 0,
    });
    adminApiMock.getLibrary.mockResolvedValue({
      id: 'library-1',
      displayName: 'Docs',
      webIngestPolicy: null,
    });
  });

  afterEach(async () => {
    if (root) {
      await act(async () => {
        root?.unmount();
      });
    }
    container.remove();
  });

  async function flushUi() {
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
  }

  async function renderPage(initialEntry = '/documents') {
    const queryClient = new QueryClient({
      defaultOptions: { queries: { retry: false, staleTime: 0, refetchOnWindowFocus: false } },
    });
    await act(async () => {
      root = createRoot(container);
      root.render(
        <QueryClientProvider client={queryClient}>
          <TooltipProvider>
            <MemoryRouter initialEntries={[initialEntry]}>
              <DocumentsPage />
              <LocationProbe />
            </MemoryRouter>
          </TooltipProvider>
        </QueryClientProvider>,
      );
    });

    await flushUi();
    await flushUi();
  }

  it('uses persisted document table controls when the URL does not override them', async () => {
    localStorage.setItem(
      getTableStateStorageKey('documents.list'),
      JSON.stringify({
        pageSize: 100,
        sort: { key: 'file_name', direction: 'asc' },
        localSort: null,
      }),
    );

    await renderPage();

    expect(documentsApiMock.list).toHaveBeenCalledWith(
      expect.objectContaining({
        limit: 100,
        sortBy: 'file_name',
        sortOrder: 'asc',
      }),
    );
  });

  it('keeps document table URL parameters stronger than persisted controls', async () => {
    localStorage.setItem(
      getTableStateStorageKey('documents.list'),
      JSON.stringify({
        pageSize: 100,
        sort: { key: 'file_name', direction: 'asc' },
        localSort: null,
      }),
    );

    await renderPage('/documents?pageSize=250&sort=status:desc');

    expect(documentsApiMock.list).toHaveBeenCalledWith(
      expect.objectContaining({
        limit: 250,
        sortBy: 'status',
        sortOrder: 'desc',
      }),
    );
  });

  it('applies a status URL filter when opened from dashboard attention', async () => {
    await renderPage('/documents?status=failed');

    expect(documentsApiMock.list).toHaveBeenCalledWith(
      expect.objectContaining({
        status: 'failed',
      }),
    );

    const failedFilter = Array.from(container.querySelectorAll('button')).find((button) =>
      button.textContent?.includes('Failed'),
    );
    expect(failedFilter?.className).toContain('text-foreground');
  });

  it('opens the editor from the table action', async () => {
    await renderPage();

    const documentRow = Array.from(container.querySelectorAll('tr')).find((row) =>
      row.textContent?.includes('inventory.xlsx'),
    );
    expect(documentRow).toBeTruthy();

    await act(async () => {
      documentRow?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });

    await flushUi();

    const editButton = Array.from(container.querySelectorAll('button')).find((button) =>
      button.textContent?.includes('Edit inventory.xlsx'),
    );
    expect(editButton).toBeTruthy();

    await act(async () => {
      editButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });

    await flushUi();

    expect(documentsApiMock.getEditorSourceText).toHaveBeenCalledWith('doc-1');
    expect(container.querySelector('[data-testid="document-editor-shell"]')).toBeTruthy();
  });

  it('opens the graph with the selected document node id from the inspector action', async () => {
    await renderPage('/documents?documentId=doc-1');
    await flushUi();

    const viewInGraphButton = Array.from(container.querySelectorAll('button')).find((button) =>
      button.textContent?.includes('View in Graph'),
    );
    expect(viewInGraphButton).toBeTruthy();

    await act(async () => {
      viewInGraphButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });
    await flushUi();

    expect(container.querySelector('[data-testid="current-location"]')?.textContent).toBe(
      '/graph?nodeId=doc-1',
    );
  });

  it('opens the inspector for a deep-linked documentId that is off the loaded page', async () => {
    // Default list page contains only `doc-1`; the deep-linked target lives
    // off-page (e.g. arrived from the Graph view's "open document" action on
    // a document buried thousands of rows deep). The page must resolve it
    // through the id-filtered list endpoint so the inspector still opens.
    documentsApiMock.list.mockImplementation(
      async (params: { ids?: string }) => {
        if (params.ids === 'doc-off-page') {
          return listPage([
            { id: 'doc-off-page', fileName: 'buried.xlsx', sourceKind: 'upload' },
          ]);
        }
        return listPage([
          { id: 'doc-1', fileName: 'inventory.xlsx', sourceKind: 'upload' },
        ]);
      },
    );

    await renderPage('/documents?documentId=doc-off-page');
    await flushUi();

    // The id-filtered list call resolved the off-page document.
    expect(documentsApiMock.list).toHaveBeenCalledWith(
      expect.objectContaining({ ids: 'doc-off-page' }),
    );

    // The inspector rendered for the deep-linked document (its action
    // buttons only mount inside the inspector panel).
    const inspectorButton = Array.from(container.querySelectorAll('button')).find((button) =>
      button.textContent?.includes('buried.xlsx'),
    );
    expect(inspectorButton).toBeTruthy();
  });

  it('saves edited markdown through the edit mutation and refreshes the document', async () => {
    await renderPage();

    const documentRow = Array.from(container.querySelectorAll('tr')).find((row) =>
      row.textContent?.includes('inventory.xlsx'),
    );
    expect(documentRow).toBeTruthy();

    await act(async () => {
      documentRow?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });

    await flushUi();

    const editButton = Array.from(container.querySelectorAll('button')).find((button) =>
      button.textContent?.includes('Edit inventory.xlsx'),
    );
    expect(editButton).toBeTruthy();

    await act(async () => {
      editButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });

    await flushUi();

    const saveButton = Array.from(container.querySelectorAll('button')).find((button) =>
      button.textContent?.includes('Save Editor'),
    );
    expect(saveButton).toBeTruthy();

    await act(async () => {
      saveButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });

    await flushUi();
    await flushUi();
    await flushUi();
    await flushUi();

    expect(documentsApiMock.edit).toHaveBeenCalledWith(
      'doc-1',
      '## Sheet1\n\n| Item | Qty |\n| --- | --- |\n| Widget | 9 |',
    );
    // After migration each list query hits documentsApiMock.list via the
    // queries stub. On mount: 2 calls (page + aggregate). After
    // loadFirstPage (invalidate): 2 more (page + aggregate refetch).
    expect(documentsApiMock.list).toHaveBeenCalledTimes(4);
    expect(documentsApiMock.get).toHaveBeenCalledWith('doc-1');
  });

  it('shows zero cost documents as "$0.000" when the list row reports cost "0"', async () => {
    // Canonical post-0.3.2 cost path: the backend list endpoint
    // attributes cost per row via `billing_execution_cost`, so the
    // frontend never calls `/billing/library-document-costs` during
    // normal page rendering. A row with a literal "0" renders as
    // "$0.000" (a billable execution landed with zero cost), a row
    // with no cost at all renders as "—".
    //
    // Post-redesign (DOC-04): the Cost column is an opt-in detail column
    // hidden by default. The per-row cost is not visible in the default view.
    documentsApiMock.list.mockResolvedValue(
      listPage([
        { id: 'doc-1', fileName: 'inventory.xlsx', sourceKind: 'upload', cost: '0' },
      ]),
    );

    await renderPage();

    // The document row is present.
    expect(container.textContent).toContain('inventory.xlsx');
    // The library-wide total cost banner stays hidden when totalCost is 0.
    expect(container.textContent).not.toContain('Library cost');
    // Per-row cost is in the opt-in detail columns — not shown by default.
    expect(container.textContent).not.toContain('$0.000');
  });

  it('shows the library-wide total cost banner alongside the per-row cost from the list payload', async () => {
    documentsApiMock.list.mockResolvedValue(
      listPage([
        { id: 'doc-1', fileName: 'inventory.xlsx', sourceKind: 'upload', cost: '1.000' },
      ]),
    );

    libraryCostSummaryFn.mockResolvedValueOnce({
      totalCost: '3.500',
      currencyCode: 'USD',
      documentCount: 2,
      providerCallCount: 4,
    });

    await renderPage();

    // The document row is present.
    expect(container.textContent).toContain('inventory.xlsx');
    // Library-wide cost banner (shown when totalCost > 0) appears in the filters bar.
    expect(container.textContent).toContain('Library cost');
    expect(container.textContent).toContain('$3.500');
    // Per-row cost ($1.000) is in opt-in detail columns — not shown by default.
    expect(container.textContent).not.toContain('$1.000');
  });

  it('renders processing progress inside the blue status badge', async () => {
    documentsApiMock.list.mockResolvedValue(
      listPage([
        {
          id: 'doc-processing',
          fileName: 'processing.pdf',
          sourceKind: 'upload',
          status: 'processing',
          readiness: 'processing',
          progressPercent: 57,
        },
      ]),
    );

    await renderPage();

    const documentRow = Array.from(container.querySelectorAll('tr')).find((row) =>
      row.textContent?.includes('processing.pdf'),
    );
    const progressBadge = documentRow?.querySelector('span[aria-label="Processing 57%"]');
    expect(documentRow).toBeTruthy();
    expect(progressBadge?.className).toContain('whitespace-nowrap');
    expect(progressBadge?.className).toContain('min-w-[9.25rem]');
    expect(documentRow?.textContent).toContain('Processing');
    expect(documentRow?.textContent).toContain('57%');
  });

  it('renders processing status as a visible zero-percent progress badge when the backend has no progress yet', async () => {
    documentsApiMock.list.mockResolvedValue(
      listPage([
        {
          id: 'doc-processing',
          fileName: 'processing.pdf',
          sourceKind: 'upload',
          status: 'processing',
          readiness: 'processing',
        },
      ]),
    );

    await renderPage();

    const documentRow = Array.from(container.querySelectorAll('tr')).find((row) =>
      row.textContent?.includes('processing.pdf'),
    );
    expect(documentRow).toBeTruthy();
    expect(documentRow?.textContent).toContain('Processing');
    expect(documentRow?.textContent).toContain('0%');
  });

  it('loads code-like documents from raw source text instead of prepared segments', async () => {
    documentsApiMock.list.mockResolvedValue(
      listPage([
        {
          id: 'doc-code',
          fileName: 'script.py',
          fileType: 'text/x-python',
          sourceKind: 'upload',
          sourceAccess: {
            kind: 'stored_document',
            href: '/v1/content/documents/doc-code/source',
          },
        },
      ]),
    );

    await renderPage();

    const documentRow = Array.from(container.querySelectorAll('tr')).find((row) =>
      row.textContent?.includes('script.py'),
    );
    expect(documentRow).toBeTruthy();

    await act(async () => {
      documentRow?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });

    await flushUi();

    const editButton = Array.from(container.querySelectorAll('button')).find((button) =>
      button.textContent?.includes('Edit script.py'),
    );
    expect(editButton).toBeTruthy();

    await act(async () => {
      editButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });

    await flushUi();

    expect(documentsApiMock.getSourceText).toHaveBeenCalledTimes(1);
    expect(documentsApiMock.getSourceText).toHaveBeenCalledWith('/v1/content/documents/doc-code/source');
  });

  it('loads plain text documents from raw source text instead of one prepared-segments page', async () => {
    documentsApiMock.list.mockResolvedValue(
      listPage([
        {
          id: 'doc-chat',
          fileName: 'chat.txt',
          fileType: 'text/plain',
          sourceKind: 'upload',
          sourceAccess: {
            kind: 'stored_document',
            href: '/v1/content/documents/doc-chat/source',
          },
        },
      ]),
    );
    documentsApiMock.getSourceText.mockResolvedValue('line 1\nline 2\nline 3\n');

    await renderPage();

    const documentRow = Array.from(container.querySelectorAll('tr')).find((row) =>
      row.textContent?.includes('chat.txt'),
    );
    expect(documentRow).toBeTruthy();

    await act(async () => {
      documentRow?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });

    await flushUi();

    const editButton = Array.from(container.querySelectorAll('button')).find((button) =>
      button.textContent?.includes('Edit chat.txt'),
    );
    expect(editButton).toBeTruthy();

    await act(async () => {
      editButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });

    await flushUi();

    expect(documentsApiMock.getSourceText).toHaveBeenCalledWith('/v1/content/documents/doc-chat/source');
    expect(documentsApiMock.getAllPreparedSegments).not.toHaveBeenCalled();
  });

  it('loads the editor source representation for plain text documents without stored source access', async () => {
    documentsApiMock.list.mockResolvedValue(
      listPage([
        {
          id: 'doc-chat',
          fileName: 'chat.txt',
          fileType: 'text/plain',
          sourceKind: 'upload',
        },
      ]),
    );

    await renderPage();

    const documentRow = Array.from(container.querySelectorAll('tr')).find((row) =>
      row.textContent?.includes('chat.txt'),
    );
    expect(documentRow).toBeTruthy();

    await act(async () => {
      documentRow?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });

    await flushUi();

    const editButton = Array.from(container.querySelectorAll('button')).find((button) =>
      button.textContent?.includes('Edit chat.txt'),
    );
    expect(editButton).toBeTruthy();

    await act(async () => {
      editButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });

    await flushUi();

    expect(documentsApiMock.getSourceText).not.toHaveBeenCalled();
    expect(documentsApiMock.getEditorSourceText).toHaveBeenCalledWith('doc-chat');
    expect(documentsApiMock.getAllPreparedSegments).not.toHaveBeenCalled();
  });

  it('shows web page as the document type for web-ingested documents', async () => {
    documentsApiMock.list.mockResolvedValue(
      listPage([
        {
          id: 'doc-web',
          fileName: 'https://ru.wikipedia.org/wiki/Test',
          fileType: 'text/html',
          sourceKind: 'web_page',
          sourceUri: 'https://ru.wikipedia.org/wiki/Test',
        },
      ]),
    );

    await renderPage();

    expect(container.textContent).toContain('Web page');
  });

  it('renders full table filenames and leaves truncation to the cell width', async () => {
    const fileName = 'weather-climate-gates-foundation.pptx';
    documentsApiMock.list.mockResolvedValue(
      listPage([{ id: 'doc-long-name', fileName, sourceKind: 'upload' }]),
    );

    await renderPage();

    const nameSpan = Array.from(container.querySelectorAll('tbody span')).find(
      (span) => span.getAttribute('title') === fileName,
    );
    expect(nameSpan).toBeTruthy();
    expect(nameSpan).toHaveTextContent(fileName);
  });
});
