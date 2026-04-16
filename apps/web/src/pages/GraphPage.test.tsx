import { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { MemoryRouter, Route, Routes, useLocation } from 'react-router-dom';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import GraphPage from '@/pages/GraphPage';

const { useAppMock, documentsApiMock, knowledgeApiMock } = vi.hoisted(() => ({
  useAppMock: vi.fn(),
  documentsApiMock: {
    get: vi.fn(),
  },
  knowledgeApiMock: {
    listDocuments: vi.fn(),
    getGraphTopology: vi.fn(),
    getEntity: vi.fn(),
  },
}));

vi.mock('@/contexts/AppContext', () => ({
  useApp: () => useAppMock(),
}));

vi.mock('@/api', () => ({
  documentsApi: documentsApiMock,
  knowledgeApi: knowledgeApiMock,
}));

vi.mock('@/components/SigmaGraph', () => ({
  // The real SigmaGraph receives the full topology and a `hiddenIds` set —
  // it applies the filter through Sigma's reducer pipeline without rebuilding
  // the Graphology instance. The test mock mirrors that contract so the
  // observable "how many nodes does the user see" still matches the old
  // filteredNodes assertion.
  default: (props: {
    nodes: Array<{ id: string; label: string }>;
    hiddenIds?: Set<string>;
    onSelect: (id: string | null) => void;
  }) => {
    const hidden = props.hiddenIds ?? new Set<string>();
    const visible = props.nodes.filter((node) => !hidden.has(node.id));
    return (
      <div data-testid="sigma-graph">
        <div data-testid="visible-node-count">{visible.length}</div>
        <div data-testid="topology-node-count">{props.nodes.length}</div>
        {visible.map((node) => (
          <button key={node.id} onClick={() => props.onSelect(node.id)} type="button">
            {node.label}
          </button>
        ))}
      </div>
    );
  },
}));

function DocumentsLocationProbe() {
  const location = useLocation();

  return <div data-testid="documents-location">{`${location.pathname}${location.search}`}</div>;
}

describe('GraphPage', () => {
  let container: HTMLDivElement;
  let root: Root | null;

  beforeEach(() => {
    vi.clearAllMocks();
    (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

    container = document.createElement('div');
    document.body.appendChild(container);
    root = null;

    useAppMock.mockReturnValue({
      activeLibrary: { id: 'library-1', name: 'Graph Library' },
      locale: 'en',
    });

    knowledgeApiMock.getGraphTopology.mockResolvedValue({
      entities: [
        {
          entityId: 'entity-1',
          canonicalLabel: 'Pipeline Stage',
          entityType: 'concept',
          supportCount: 2,
        },
      ],
      relations: [],
      documents: [
        {
          document_id: 'doc-1',
          title: 'data_pipeline.py',
          document_state: 'graph_ready',
        },
        {
          document_id: 'doc-2',
          title: 'etl_service.py',
          document_state: 'graph_ready',
        },
      ],
      documentLinks: [
        { documentId: 'doc-1', targetNodeId: 'entity-1', supportCount: 1 },
        { documentId: 'doc-2', targetNodeId: 'entity-1', supportCount: 1 },
      ],
    });
    knowledgeApiMock.getEntity.mockResolvedValue({
      entity: {
        entityId: 'entity-1',
        canonicalLabel: 'Pipeline Stage',
        entityType: 'concept',
        supportCount: 2,
      },
    });
    documentsApiMock.get.mockResolvedValue({
      fileName: 'data_pipeline.py',
      head: {
        document_summary: 'Canonical summary for the selected document.',
      },
      readinessSummary: {
        readinessKind: 'graph_ready',
        activityStatus: 'ready',
        graphCoverageKind: 'graph_ready',
      },
      activeRevision: {
        mime_type: 'application/octet-stream',
        byte_size: 4096,
        revision_number: 1,
      },
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

  // The graph search input is debounced by 250 ms before it lands in the
  // hiddenIds set the SigmaGraph mock observes. Tests that exercise the
  // search filter must wait at least one debounce tick.
  async function flushSearchDebounce() {
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 280));
    });
  }

  async function renderPage() {
    await act(async () => {
      root = createRoot(container);
      root.render(
        <MemoryRouter initialEntries={['/graph']}>
          <Routes>
            <Route path="/graph" element={<GraphPage />} />
            <Route path="/documents" element={<DocumentsLocationProbe />} />
          </Routes>
        </MemoryRouter>,
      );
    });

    await flushUi();
    await flushUi();
  }

  function findButton(text: string) {
    return Array.from(container.querySelectorAll('button')).find((button) =>
      button.textContent?.trim() === text,
    );
  }

  function setInputValue(input: HTMLInputElement, value: string) {
    const descriptor = Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype, 'value');
    descriptor?.set?.call(input, value);
    input.dispatchEvent(new Event('input', { bubbles: true }));
  }

  it('clears the text filter and restores the full graph with the global reset action', async () => {
    await renderPage();

    const visibleNodeCount = () => container.querySelector('[data-testid="visible-node-count"]')?.textContent;
    const searchInput = container.querySelector('input') as HTMLInputElement | null;

    expect(searchInput).toBeTruthy();
    expect(knowledgeApiMock.getGraphTopology).toHaveBeenCalledTimes(1);
    expect(visibleNodeCount()).toBe('3');

    const documentNodeButton = findButton('data_pipeline.py');
    expect(documentNodeButton).toBeTruthy();

    await act(async () => {
      documentNodeButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });
    await flushUi();

    expect(container.textContent).not.toContain('Find Similar');

    await act(async () => {
      if (searchInput) setInputValue(searchInput, 'data_pipeline.py');
    });
    await flushSearchDebounce();
    await flushUi();

    expect(visibleNodeCount()).toBe('1');

    const clearButton = findButton('Clear');
    expect(clearButton).toBeTruthy();

    await act(async () => {
      clearButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });
    await flushSearchDebounce();
    await flushUi();

    expect(searchInput).toHaveValue('');
    expect(visibleNodeCount()).toBe('3');
  });

  it('does not refetch topology or rebuild node identity when search filter changes', async () => {
    // Regression guard for the wave-3 perf fix: filters must be applied
    // via SigmaGraph's reducer pipeline, so typing in the search box must
    // NOT trigger a topology refetch AND must keep the `nodes` prop stable
    // (same length = full topology). This is what makes the filter cheap
    // at 100k-node scale.
    await renderPage();

    const topologyCount = () =>
      container.querySelector('[data-testid="topology-node-count"]')?.textContent;
    const visibleNodeCount = () =>
      container.querySelector('[data-testid="visible-node-count"]')?.textContent;
    const searchInput = container.querySelector('input') as HTMLInputElement | null;

    expect(knowledgeApiMock.getGraphTopology).toHaveBeenCalledTimes(1);
    expect(topologyCount()).toBe('3');
    expect(visibleNodeCount()).toBe('3');

    await act(async () => {
      if (searchInput) setInputValue(searchInput, 'data_pipeline.py');
    });
    await flushSearchDebounce();
    await flushUi();

    // Filter narrows what the user sees, but topology stays whole and the
    // backend is not re-hit.
    expect(visibleNodeCount()).toBe('1');
    expect(topologyCount()).toBe('3');
    expect(knowledgeApiMock.getGraphTopology).toHaveBeenCalledTimes(1);
  });

  it('expands and collapses overflowing subtype groups with explicit controls', async () => {
    knowledgeApiMock.getGraphTopology.mockResolvedValue({
      entities: [
        ...Array.from({ length: 14 }, (_, index) => ({
          entityId: `artifact-${index + 1}`,
          canonicalLabel: `Artifact ${index + 1}`,
          entityType: 'artifact',
          entitySubType: `artifact_sub_${index + 1}`,
          supportCount: 1,
        })),
      ],
      relations: [],
      documents: [],
      documentLinks: [],
    });

    await renderPage();

    expect(container.textContent).toContain('Show all (+2)');
    expect(container.textContent).not.toContain('artifact_sub_14 1');

    const showAllSubtypesButton = findButton('Show all (+2)');
    expect(showAllSubtypesButton).toBeTruthy();

    await act(async () => {
      showAllSubtypesButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });
    await flushUi();

    expect(container.textContent).toContain('artifact_sub_14 1');

    const hideSubtypesButton = findButton('Hide');
    expect(hideSubtypesButton).toBeTruthy();

    await act(async () => {
      hideSubtypesButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });
    await flushUi();

    expect(container.textContent).toContain('Show all (+2)');
    expect(container.textContent).not.toContain('artifact_sub_14 1');
  });

  it('shows the recommended layout as a toolbar action after switching away from it', async () => {
    await renderPage();

    const bandsButton = Array.from(container.querySelectorAll('button')).find((button) =>
      button.getAttribute('aria-label') === 'Bands',
    );
    expect(bandsButton).toBeTruthy();

    await act(async () => {
      bandsButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });
    await flushUi();

    const recommendedButton = Array.from(container.querySelectorAll('button')).find((button) =>
      button.textContent?.includes('Recommended:') && button.textContent.includes('Sectors'),
    );
    expect(recommendedButton).toBeTruthy();

    await act(async () => {
      recommendedButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });
    await flushUi();

    expect(
      Array.from(container.querySelectorAll('button')).find((button) =>
        button.textContent?.includes('Recommended:'),
      ),
    ).toBeUndefined();
  });

  it('shows the selected entity subtype in the detail panel', async () => {
    knowledgeApiMock.getEntity.mockResolvedValue({
      entity: {
        entityId: 'entity-1',
        canonicalLabel: 'Pipeline Stage',
        entityType: 'concept',
        entitySubType: 'pipeline_stage',
        supportCount: 2,
      },
    });

    await renderPage();

    const entityNodeButton = findButton('Pipeline Stage');
    expect(entityNodeButton).toBeTruthy();

    await act(async () => {
      entityNodeButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });
    await flushUi();
    await flushUi();

    expect(container.textContent).toContain('pipeline_stage');
    expect(container.textContent).toContain('Sub-type');
  });

  it('shows and filters the no-sub-type legend bucket only when the type also has real sub-types', async () => {
    knowledgeApiMock.getGraphTopology.mockResolvedValue({
      entities: [
        {
          entityId: 'entity-1',
          canonicalLabel: 'Pipeline Stage',
          entityType: 'concept',
          entitySubType: 'pipeline_stage',
          supportCount: 1,
        },
        {
          entityId: 'entity-2',
          canonicalLabel: 'Untyped Concept',
          entityType: 'concept',
          supportCount: 1,
        },
      ],
      relations: [],
      documents: [
        {
          document_id: 'doc-1',
          title: 'data_pipeline.py',
          document_state: 'graph_ready',
        },
        {
          document_id: 'doc-2',
          title: 'etl_service.py',
          document_state: 'graph_ready',
        },
      ],
      documentLinks: [],
    });

    await renderPage();

    expect(container.textContent).toContain('No sub-type 1');
    expect(container.textContent).not.toContain('No sub-type 2');

    const noSubtypeButton = Array.from(container.querySelectorAll('button')).find((button) =>
      button.textContent?.includes('No sub-type 1'),
    );
    expect(noSubtypeButton).toBeTruthy();

    await act(async () => {
      noSubtypeButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });
    await flushUi();

    expect(container.querySelector('[data-testid="visible-node-count"]')?.textContent).toBe('3');
  });

  it('uses a wrapping property layout for long document values', async () => {
    documentsApiMock.get.mockResolvedValue({
      fileName: 'Week 1.docx',
      readinessSummary: {
        readinessKind: 'graph_sparse',
        activityStatus: 'ready',
        graphCoverageKind: 'graph_sparse',
      },
      activeRevision: {
        mime_type: 'application/vnd.openxmlformats-officedocument.wordprocessingml.document',
        byte_size: 4096,
        revision_number: 1,
      },
    });

    await renderPage();

    const documentNodeButton = findButton('data_pipeline.py');
    expect(documentNodeButton).toBeTruthy();

    await act(async () => {
      documentNodeButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });
    await flushUi();
    await flushUi();

    const formatValue = Array.from(container.querySelectorAll('span')).find((span) =>
      span.textContent === 'application/vnd.openxmlformats-officedocument.wordprocessingml.document',
    );
    expect(formatValue).toBeTruthy();
    expect(formatValue?.className).toContain('[overflow-wrap:anywhere]');
    expect(formatValue?.parentElement?.className).toContain('grid');
  });

  it('opens the documents page with the selected document id', async () => {
    await renderPage();

    const documentNodeButton = findButton('data_pipeline.py');
    expect(documentNodeButton).toBeTruthy();

    await act(async () => {
      documentNodeButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });
    await flushUi();
    await flushUi();

    const viewDocumentButton = Array.from(container.querySelectorAll('button')).find((button) =>
      button.textContent?.includes('View Document'),
    );
    expect(viewDocumentButton).toBeTruthy();

    await act(async () => {
      viewDocumentButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });
    await flushUi();

    expect(container.querySelector('[data-testid="documents-location"]')?.textContent).toBe(
      '/documents?documentId=doc-1',
    );
  });

  it('shows the canonical document summary instead of readiness state', async () => {
    await renderPage();

    const documentNodeButton = findButton('data_pipeline.py');
    expect(documentNodeButton).toBeTruthy();

    await act(async () => {
      documentNodeButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });
    await flushUi();
    await flushUi();

    expect(container.textContent).toContain('Canonical summary for the selected document.');
  });

  it('groups neighbours of the selected entity into documents and concepts via the adjacency index', async () => {
    // Topology with one concept entity connected to two documents via the
    // document-link projection. The inspector must walk the adjacency index
    // and surface both documents under "Source documents" without re-scanning
    // every edge per render.
    knowledgeApiMock.getGraphTopology.mockResolvedValue({
      entities: [
        {
          entityId: 'entity-1',
          canonicalLabel: 'Pipeline Stage',
          entityType: 'concept',
          supportCount: 2,
        },
      ],
      relations: [],
      documents: [
        { document_id: 'doc-1', title: 'data_pipeline.py', document_state: 'graph_ready' },
        { document_id: 'doc-2', title: 'etl_service.py', document_state: 'graph_ready' },
      ],
      documentLinks: [
        { documentId: 'doc-1', targetNodeId: 'entity-1', supportCount: 1 },
        { documentId: 'doc-2', targetNodeId: 'entity-1', supportCount: 1 },
      ],
    });

    await renderPage();

    const entityButton = findButton('Pipeline Stage');
    expect(entityButton).toBeTruthy();

    await act(async () => {
      entityButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });
    await flushUi();
    await flushUi();

    // Both source documents must appear in the inspector as clickable
    // neighbor rows. The connection counter in the header must also show
    // the full neighbour count (2), not a truncated projection.
    expect(container.textContent).toContain('Source Documents (2)');
    expect(container.textContent).toContain('data_pipeline.py');
    expect(container.textContent).toContain('etl_service.py');
    expect(container.textContent).toMatch(/2\s+connections/);
  });
});
