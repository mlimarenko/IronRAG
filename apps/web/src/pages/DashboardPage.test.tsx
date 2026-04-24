import { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { MemoryRouter, Route, Routes, useLocation } from 'react-router-dom';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import DashboardPage from '@/pages/DashboardPage';

const { useAppMock, dashboardApiMock } = vi.hoisted(() => ({
  useAppMock: vi.fn(),
  dashboardApiMock: {
    getLibraryDashboard: vi.fn(),
  },
}));

vi.mock('@/contexts/AppContext', () => ({
  useApp: () => useAppMock(),
}));

vi.mock('@/api', () => ({
  dashboardApi: dashboardApiMock,
}));

function LocationProbe() {
  const location = useLocation();
  return <div data-testid="destination">{`${location.pathname}${location.search}`}</div>;
}

function sampleDashboard() {
  return {
    overview: {
      totalDocuments: 12,
      readyDocuments: 8,
      processingDocuments: 2,
      failedDocuments: 1,
      graphSparseDocuments: 1,
    },
    metrics: [
      { key: 'in_flight', value: '3', level: 'info' },
    ],
    recentDocuments: [
      {
        id: 'doc-active',
        fileName: 'active.pdf',
        fileSize: 2048,
        uploadedAt: new Date(Date.now() - 60_000).toISOString(),
        readiness: 'graph_ready',
        stageLabel: null,
        failureMessage: null,
        canRetry: false,
        preparedSegmentCount: 12,
        technicalFactCount: 4,
      },
      {
        id: 'doc-failed',
        fileName: 'broken.pdf',
        fileSize: 1024,
        uploadedAt: new Date(Date.now() - 120_000).toISOString(),
        readiness: 'failed',
        stageLabel: null,
        failureMessage: 'parser_error',
        canRetry: true,
        preparedSegmentCount: 0,
        technicalFactCount: 0,
      },
    ],
    recentWebRuns: [
      {
        runId: 'run-old',
        runState: 'completed',
        seedUrl: 'https://example.com/docs',
        counts: { discovered: 10, eligible: 10, processed: 8, queued: 0, processing: 0, blocked: 1, failed: 1 },
        lastActivityAt: '2026-04-09T09:00:00Z',
      },
      {
        runId: 'run-latest',
        runState: 'processing',
        seedUrl: 'https://example.com/api',
        counts: { discovered: 15, eligible: 15, processed: 5, queued: 3, processing: 2, blocked: 0, failed: 0 },
        lastActivityAt: '2026-04-10T09:00:00Z',
      },
    ],
    graph: {
      status: 'ready',
      warning: null,
      nodeCount: 42,
      edgeCount: 101,
      graphReadyDocumentCount: 7,
      graphSparseDocumentCount: 1,
      typedFactDocumentCount: 5,
      updatedAt: '2026-04-10T12:00:00Z',
    },
    attention: [
      {
        code: 'failed_documents',
        title: 'custom title ignored',
        detail: 'custom detail ignored',
        routePath: '/ignored',
        level: 'error',
      },
    ],
  };
}

describe('DashboardPage integration', () => {
  let container: HTMLDivElement;
  let root: Root | null;

  beforeEach(() => {
    vi.clearAllMocks();
    container = document.createElement('div');
    document.body.appendChild(container);
    root = null;

    useAppMock.mockReturnValue({
      activeLibrary: { id: 'library-1', name: 'Main' },
    });
    dashboardApiMock.getLibraryDashboard.mockResolvedValue(sampleDashboard());
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

  async function renderPage() {
    await act(async () => {
      root = createRoot(container);
      root.render(
        <MemoryRouter initialEntries={['/']}>
          <Routes>
            <Route path="/" element={<DashboardPage />} />
            <Route path="/documents" element={<LocationProbe />} />
            <Route path="/graph" element={<LocationProbe />} />
          </Routes>
        </MemoryRouter>,
      );
    });
    await flushUi();
    await flushUi();
  }

  function findButton(text: string) {
    return Array.from(container.querySelectorAll('button')).find((b) =>
      b.textContent?.includes(text),
    );
  }

  it('fetches the dashboard for the active library and renders summary tiles', async () => {
    await renderPage();

    expect(dashboardApiMock.getLibraryDashboard).toHaveBeenCalledTimes(1);
    expect(dashboardApiMock.getLibraryDashboard).toHaveBeenCalledWith('library-1');

    // Summary cards show derived counts, not raw backend values.
    expect(container.textContent).toContain('12'); // total documents
    expect(container.textContent).toContain('58%'); // 7/12 graph ready ≈ 58%
    expect(container.textContent).toContain('Active Operations');
  });

  it('localizes attention entries from their canonical code, not the backend title', async () => {
    await renderPage();

    // The backend sent `title: 'custom title ignored'` — the UI must NOT echo it
    // for known codes; it must use the translated `attentionTitles.failed_documents`.
    expect(container.textContent).not.toContain('custom title ignored');
    expect(container.textContent).toContain('Failed documents');
  });

  it('surfaces the most recent web run, not the first in the list', async () => {
    await renderPage();

    // Latest run selection is by lastActivityAt desc, so `run-latest` wins.
    expect(container.textContent).toContain('example.com/api');
    expect(container.textContent).not.toContain('example.com/docs');
  });

  it('navigates to documents with the deep-link for a recent document card', async () => {
    await renderPage();

    const card = findButton('broken.pdf');
    expect(card).toBeTruthy();

    await act(async () => {
      card?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });
    await flushUi();

    expect(container.querySelector('[data-testid="destination"]')?.textContent).toBe(
      '/documents?documentId=doc-failed',
    );
  });

  it('refreshes on demand without rebuilding the whole page', async () => {
    await renderPage();
    expect(dashboardApiMock.getLibraryDashboard).toHaveBeenCalledTimes(1);

    const refresh = Array.from(container.querySelectorAll('button')).find((b) =>
      b.textContent?.trim().toLowerCase().includes('refresh'),
    );
    expect(refresh).toBeTruthy();

    await act(async () => {
      refresh?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });
    await flushUi();
    await flushUi();

    expect(dashboardApiMock.getLibraryDashboard).toHaveBeenCalledTimes(2);
  });

  it('renders the no-library empty state when no active library is set', async () => {
    useAppMock.mockReturnValue({ activeLibrary: null });
    await renderPage();

    expect(dashboardApiMock.getLibraryDashboard).not.toHaveBeenCalled();
    expect(container.textContent).toContain('No library selected');
  });
});
