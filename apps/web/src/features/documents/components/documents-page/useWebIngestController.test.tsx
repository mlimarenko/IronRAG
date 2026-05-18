import { act } from 'react';
import { QueryClient, QueryClientProvider, useQuery } from '@tanstack/react-query';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { CatalogLibraryResponse } from '@/shared/api';
import { queries } from '@/shared/api';
import type { Library } from '@/shared/types';

import { extractWebIngestPolicy } from './documentsPageState';
import { useWebIngestController } from './useWebIngestController';

const { adminApiMock, documentsApiMock, toastErrorMock } = vi.hoisted(() => ({
  adminApiMock: {
    updateWebIngestPolicy: vi.fn(),
  },
  documentsApiMock: {
    createWebIngestRun: vi.fn(),
    listWebRuns: vi.fn(),
  },
  toastErrorMock: vi.fn(),
}));

vi.mock('sonner', () => ({
  toast: {
    error: toastErrorMock,
    success: vi.fn(),
  },
}));

vi.mock('@/shared/api', () => ({
  adminApi: adminApiMock,
  documentsApi: documentsApiMock,
  queries: {
    getCatalogLibraryOptions: (input: { path: { libraryId: string } }) => ({
      queryKey: ['mockedCatalogLibrary', input.path.libraryId],
      queryFn: async () => {
        throw new Error('unexpected catalog fetch');
      },
    }),
  },
}));

const activeLibrary: Library = {
  id: 'library-1',
  workspaceId: 'ws-1',
  name: 'Docs',
  createdAt: '2026-04-10T10:00:00Z',
  ingestionReady: true,
  queryReady: true,
  missingBindingPurposes: [],
};

const initialLibrary: CatalogLibraryResponse = {
  id: 'library-1',
  workspaceId: 'ws-1',
  displayName: 'Docs',
  description: null,
  extractionPrompt: null,
  ingestionReadiness: {
    ready: true,
    missingBindingPurposes: [],
  },
  lifecycleState: 'active',
  recognitionPolicy: {
    rasterImageEngine: 'native',
  },
  slug: 'docs',
  webIngestPolicy: {
    crawlFilter: {
      allowPatterns: [],
      blockPatterns: [],
    },
    materializationFilter: {
      allowPatterns: [],
      blockPatterns: [],
    },
  },
};

function t(key: string, options?: Record<string, unknown>) {
  return options?.error ? `${key}: ${String(options.error)}` : key;
}

function Harness({
  loadFirstPage,
  refreshWebRuns,
}: {
  loadFirstPage: () => Promise<void>;
  refreshWebRuns: () => Promise<void>;
}) {
  const libraryQuery = useQuery({
    ...queries.getCatalogLibraryOptions({ path: { libraryId: activeLibrary.id } }),
    initialData: initialLibrary,
    staleTime: Infinity,
  });
  const loadedWebIngestPolicy = extractWebIngestPolicy(libraryQuery.data);
  const controller = useWebIngestController({
    activeLibrary,
    errorMessage: (error, fallback) =>
      error instanceof Error ? error.message : fallback,
    fetchLibraryWebIngestPolicy: async () => loadedWebIngestPolicy,
    libraryPolicyData: libraryQuery.data,
    libraryPolicyLoading: false,
    loadedWebIngestPolicy,
    loadFirstPage,
    refreshWebRuns,
    t,
    webRuns: [],
    webRunsRefreshing: false,
  });

  return (
    <div>
      <div data-testid="saved-count">
        {libraryQuery.data.webIngestPolicy?.crawlFilter?.allowPatterns?.length ?? 0}
      </div>
      <button
        onClick={() => {
          controller.setSeedUrl('docs.example.com');
          controller.setCrawlAllowPatternsText('url_prefix:https://docs.example.com');
        }}
      >
        Draft
      </button>
      <button onClick={() => void controller.startWebIngest()}>Start</button>
    </div>
  );
}

describe('useWebIngestController optimistic policy save', () => {
  let container: HTMLDivElement;
  let queryClient: QueryClient;
  let root: Root | null;

  beforeEach(() => {
    vi.clearAllMocks();
    container = document.createElement('div');
    document.body.appendChild(container);
    root = createRoot(container);
    queryClient = new QueryClient({
      defaultOptions: { queries: { retry: false, staleTime: Infinity } },
    });
    queryClient.setQueryData(
      queries.getCatalogLibraryOptions({ path: { libraryId: activeLibrary.id } }).queryKey,
      initialLibrary,
    );
  });

  afterEach(async () => {
    await act(async () => {
      root?.unmount();
    });
    queryClient.clear();
    container.remove();
    root = null;
  });

  async function flushUi() {
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
  }

  async function renderHarness() {
    await act(async () => {
      root?.render(
        <QueryClientProvider client={queryClient}>
          <Harness loadFirstPage={vi.fn()} refreshWebRuns={vi.fn()} />
        </QueryClientProvider>,
      );
    });
    await flushUi();
  }

  it('shows the saved policy optimistically and rolls back with a toast on failure', async () => {
    let rejectPolicy!: (reason: Error) => void;
    adminApiMock.updateWebIngestPolicy.mockReturnValue(
      new Promise((_resolve, reject) => {
        rejectPolicy = reject;
      }),
    );

    await renderHarness();

    expect(container.querySelector('[data-testid="saved-count"]')).toHaveTextContent('0');

    const draftButton = Array.from(container.querySelectorAll('button')).find((button) =>
      button.textContent?.includes('Draft'),
    );
    await act(async () => {
      draftButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });
    await flushUi();

    const startButton = Array.from(container.querySelectorAll('button')).find((button) =>
      button.textContent?.includes('Start'),
    );
    await act(async () => {
      startButton?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });
    await flushUi();

    expect(container.querySelector('[data-testid="saved-count"]')).toHaveTextContent('1');

    await act(async () => {
      rejectPolicy(new Error('policy unavailable'));
    });
    await flushUi();
    await flushUi();

    expect(container.querySelector('[data-testid="saved-count"]')).toHaveTextContent('0');
    expect(toastErrorMock).toHaveBeenCalledWith(
      expect.stringContaining('policy unavailable'),
    );
  });
});
