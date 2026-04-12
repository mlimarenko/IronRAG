import { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { MemoryRouter } from 'react-router-dom';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import DocumentsPage from '@/pages/DocumentsPage';

const { useAppMock, documentsApiMock, billingApiMock, apiFetchMock } = vi.hoisted(() => ({
  useAppMock: vi.fn(),
  documentsApiMock: {
    list: vi.fn(),
    get: vi.fn(),
    getSourceText: vi.fn(),
    upload: vi.fn(),
    delete: vi.fn(),
    reprocess: vi.fn(),
    edit: vi.fn(),
    replace: vi.fn(),
    getPreparedSegments: vi.fn(),
    getTechnicalFacts: vi.fn(),
  },
  billingApiMock: {
    getLibraryDocumentCosts: vi.fn(),
  },
  apiFetchMock: vi.fn(),
}));

vi.mock('@/contexts/AppContext', () => ({
  useApp: () => useAppMock(),
}));

vi.mock('@/api', () => ({
  documentsApi: documentsApiMock,
  billingApi: billingApiMock,
  apiFetch: apiFetchMock,
}));

vi.mock('@/pages/documents/DocumentsPageHeader', () => ({
  DocumentsPageHeader: () => null,
}));

vi.mock('@/pages/documents/DocumentsInspectorPanel', () => ({
  DocumentsInspectorPanel: (props: { selectedDoc?: { fileName?: string } | null; onEdit: () => void }) =>
    props.selectedDoc ? (
      <button onClick={() => props.onEdit()}>
        Edit {props.selectedDoc.fileName}
      </button>
    ) : null,
}));

vi.mock('@/pages/documents/DocumentsOverlays', () => ({
  DocumentsOverlays: () => null,
}));

vi.mock('@/pages/documents/editor/DocumentEditorShell', () => ({
  DocumentEditorShell: (props: { open: boolean; documentName: string; onSave: (markdown: string) => void }) =>
    props.open ? (
      <div data-testid="document-editor-shell">
        <span>{props.documentName}</span>
        <button onClick={() => props.onSave('## Sheet1\n\n| Item | Qty |\n| --- | --- |\n| Widget | 9 |')}>
          Save Editor
        </button>
      </div>
    ) : null,
}));

describe('DocumentsPage', () => {
  let container: HTMLDivElement;
  let root: Root | null;

  beforeEach(() => {
    vi.clearAllMocks();
    container = document.createElement('div');
    document.body.appendChild(container);
    root = null;

    useAppMock.mockReturnValue({
      activeLibrary: { id: 'library-1', name: 'Docs' },
      locale: 'en',
    });

    documentsApiMock.list.mockResolvedValue([
      {
        fileName: 'inventory.xlsx',
        document: {
          id: 'doc-1',
          external_key: 'inventory',
          created_at: '2026-04-10T12:00:00Z',
        },
        activeRevision: {
          title: 'inventory.xlsx',
          mime_type: 'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet',
          byte_size: 2048,
          content_source_kind: 'upload',
        },
        readinessSummary: {
          readinessKind: 'graph_ready',
          activityStatus: 'completed',
        },
        pipeline: {
          latest_job: {
            queue_state: 'completed',
            current_stage: 'extracting_graph',
            retryable: false,
          },
        },
      },
    ]);
    documentsApiMock.get.mockResolvedValue({
      fileName: 'inventory.xlsx',
      document: {
        id: 'doc-1',
        external_key: 'inventory',
        created_at: '2026-04-10T12:00:00Z',
      },
      activeRevision: {
        title: 'inventory.xlsx',
        mime_type: 'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet',
        byte_size: 2048,
        content_source_kind: 'upload',
      },
      readinessSummary: {
        readinessKind: 'graph_ready',
        activityStatus: 'completed',
      },
      pipeline: {
        latest_job: {
          queue_state: 'completed',
          current_stage: 'extracting_graph',
          retryable: false,
        },
      },
    });
    documentsApiMock.getPreparedSegments.mockResolvedValue([
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
    documentsApiMock.edit.mockResolvedValue({ documentId: 'doc-1' });
    billingApiMock.getLibraryDocumentCosts.mockResolvedValue([]);
    apiFetchMock.mockResolvedValue([]);
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
      await new Promise(resolve => setTimeout(resolve, 0));
    });
  }

  async function renderPage() {
    await act(async () => {
      root = createRoot(container);
      root.render(
        <MemoryRouter initialEntries={['/documents']}>
          <DocumentsPage />
        </MemoryRouter>,
      );
    });

    await flushUi();
    await flushUi();
  }

  it('opens the editor from the table action', async () => {
    await renderPage();

    const documentRow = Array.from(container.querySelectorAll('tr')).find(row =>
      row.textContent?.includes('inventory.xlsx'),
    );
    expect(documentRow).toBeTruthy();

    await act(async () => {
      documentRow?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });

    await flushUi();

    const editButton = Array.from(container.querySelectorAll('button')).find(button =>
      button.textContent?.includes('Edit inventory.xlsx'),
    );
    expect(editButton).toBeTruthy();

    await act(async () => {
      editButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });

    await flushUi();

    expect(documentsApiMock.getPreparedSegments).toHaveBeenCalledWith('doc-1');
    expect(container.querySelector('[data-testid="document-editor-shell"]')).toBeTruthy();
  });

  it('saves edited markdown through the edit mutation and refreshes the document', async () => {
    await renderPage();

    const documentRow = Array.from(container.querySelectorAll('tr')).find(row =>
      row.textContent?.includes('inventory.xlsx'),
    );
    expect(documentRow).toBeTruthy();

    await act(async () => {
      documentRow?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });

    await flushUi();

    const editButton = Array.from(container.querySelectorAll('button')).find(button =>
      button.textContent?.includes('Edit inventory.xlsx'),
    );
    expect(editButton).toBeTruthy();

    await act(async () => {
      editButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });

    await flushUi();

    const saveButton = Array.from(container.querySelectorAll('button')).find(button =>
      button.textContent?.includes('Save Editor'),
    );
    expect(saveButton).toBeTruthy();

    await act(async () => {
      saveButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });

    await flushUi();
    await flushUi();

    expect(documentsApiMock.edit).toHaveBeenCalledWith(
      'doc-1',
      '## Sheet1\n\n| Item | Qty |\n| --- | --- |\n| Widget | 9 |',
    );
    expect(documentsApiMock.list).toHaveBeenCalledTimes(2);
    expect(documentsApiMock.get).toHaveBeenCalledWith('doc-1');
  });

  it('shows zero cost documents when billing returns an explicit zero row', async () => {
    billingApiMock.getLibraryDocumentCosts.mockResolvedValue([
      { documentId: 'doc-1', totalCost: '0', currencyCode: 'USD', providerCallCount: 0 },
    ]);

    await renderPage();

    expect(container.textContent).toContain('$0.000');
  });

  it('loads code-like documents from raw source text instead of prepared segments', async () => {
    documentsApiMock.list.mockResolvedValue([
      {
        fileName: 'script.py',
        document: {
          id: 'doc-code',
          external_key: 'script',
          created_at: '2026-04-10T12:00:00Z',
        },
        activeRevision: {
          title: 'script.py',
          mime_type: 'text/x-python',
          byte_size: 512,
          content_source_kind: 'upload',
          source_uri: '/v1/content/documents/doc-code/source',
        },
        sourceAccess: { kind: 'stored_document', href: '/v1/content/documents/doc-code/source' },
        readinessSummary: {
          readinessKind: 'graph_ready',
          activityStatus: 'completed',
        },
        pipeline: {
          latest_job: {
            queue_state: 'completed',
            current_stage: 'extracting_graph',
            retryable: false,
          },
        },
      },
    ]);
    documentsApiMock.get.mockResolvedValue({
      fileName: 'script.py',
      document: {
        id: 'doc-code',
        external_key: 'script',
        created_at: '2026-04-10T12:00:00Z',
      },
      activeRevision: {
        title: 'script.py',
        mime_type: 'text/x-python',
        byte_size: 512,
        content_source_kind: 'upload',
        source_uri: '/v1/content/documents/doc-code/source',
      },
      sourceAccess: { kind: 'stored_document', href: '/v1/content/documents/doc-code/source' },
      readinessSummary: {
        readinessKind: 'graph_ready',
        activityStatus: 'completed',
      },
      pipeline: {
        latest_job: {
          queue_state: 'completed',
          current_stage: 'extracting_graph',
          retryable: false,
        },
      },
    });

    await renderPage();

    const documentRow = Array.from(container.querySelectorAll('tr')).find(row =>
      row.textContent?.includes('script.py'),
    );
    expect(documentRow).toBeTruthy();

    await act(async () => {
      documentRow?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });

    await flushUi();

    const editButton = Array.from(container.querySelectorAll('button')).find(button =>
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

  it('shows web page as the document type for web-ingested documents', async () => {
    documentsApiMock.list.mockResolvedValue([
      {
        fileName: 'https://ru.wikipedia.org/wiki/Test',
        document: {
          id: 'doc-web',
          external_key: 'https://ru.wikipedia.org/wiki/Test',
          created_at: '2026-04-10T12:00:00Z',
        },
        activeRevision: {
          title: 'index.php',
          mime_type: 'text/html',
          byte_size: 2048,
          content_source_kind: 'web_page',
          source_uri: 'https://ru.wikipedia.org/wiki/Test',
        },
        readinessSummary: {
          readinessKind: 'graph_ready',
          activityStatus: 'completed',
        },
        pipeline: {
          latest_job: {
            queue_state: 'completed',
            current_stage: 'extracting_graph',
            retryable: false,
          },
        },
      },
    ]);

    await renderPage();

    expect(container.textContent).toContain('Web page');
  });
});
