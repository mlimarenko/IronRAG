import { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { MemoryRouter } from 'react-router-dom';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import AdminPage from '@/pages/AdminPage';

const {
  useAppMock,
  adminApiMock,
  dashboardApiMock,
  librarySnapshotApiMock,
  queryApiMock,
} = vi.hoisted(() => ({
  useAppMock: vi.fn(),
  adminApiMock: {
    listTokens: vi.fn(),
    listWorkspaces: vi.fn(),
    listLibraries: vi.fn(),
    mintToken: vi.fn(),
    revokeToken: vi.fn(),
    listProviders: vi.fn(),
    listModels: vi.fn(),
    listCredentials: vi.fn(),
    listPresets: vi.fn(),
    listBindings: vi.fn(),
    listPrices: vi.fn(),
    createPriceOverride: vi.fn(),
    listAuditEvents: vi.fn(),
  },
  dashboardApiMock: {
    getLibraryState: vi.fn(),
  },
  librarySnapshotApiMock: {
    export: vi.fn(),
    import: vi.fn(),
  },
  queryApiMock: {
    getAssistantSystemPrompt: vi.fn(),
  },
}));

vi.mock('@/contexts/AppContext', () => ({
  useApp: () => useAppMock(),
}));

vi.mock('@/api', () => ({
  adminApi: adminApiMock,
  dashboardApi: dashboardApiMock,
  librarySnapshotApi: librarySnapshotApiMock,
  queryApi: queryApiMock,
}));

// AiConfigurationPanel is heavy (937 lines) and not what these integration
// tests are validating — they check tab routing and the orchestrator shell.
vi.mock('@/components/admin/AiConfigurationPanel', () => ({
  default: () => <div data-testid="ai-panel">AI panel</div>,
}));

describe('AdminPage integration', () => {
  let container: HTMLDivElement;
  let root: Root | null;

  beforeEach(() => {
    vi.clearAllMocks();
    container = document.createElement('div');
    document.body.appendChild(container);
    root = null;

    useAppMock.mockReturnValue({
      activeWorkspace: { id: 'ws-1', name: 'Workspace 1' },
      activeLibrary: { id: 'library-1', name: 'Library 1' },
      locale: 'en',
      setLocale: vi.fn(),
    });

    adminApiMock.listTokens.mockResolvedValue([
      {
        id: 'principal-1',
        principalId: 'principal-1',
        label: 'Ops token',
        tokenPrefix: 'irr_abc',
        status: 'active',
        workspaceId: 'ws-1',
      },
    ]);
    adminApiMock.listProviders.mockResolvedValue([]);
    adminApiMock.listModels.mockResolvedValue([]);
    adminApiMock.listPrices.mockResolvedValue([]);
    adminApiMock.listAuditEvents.mockResolvedValue({ items: [], total: 0, limit: 50, offset: 0 });
    adminApiMock.listWorkspaces.mockResolvedValue([
      { id: 'ws-1', displayName: 'Workspace 1' },
    ]);
    adminApiMock.listLibraries.mockResolvedValue([
      { id: 'library-1', displayName: 'Library 1' },
    ]);
    dashboardApiMock.getLibraryState.mockResolvedValue({
      state: {
        queueDepth: 0,
        runningAttempts: 0,
        readableDocumentCount: 0,
        failedDocumentCount: 0,
        degradedState: 'healthy',
        knowledgeGenerationState: 'graph_ready',
        lastRecomputedAt: '2026-04-10T10:00:00Z',
      },
      warnings: [],
    });
    queryApiMock.getAssistantSystemPrompt.mockResolvedValue({
      rendered: '# MCP system prompt',
      template: '# template',
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

  async function renderPage(initialPath = '/admin') {
    await act(async () => {
      root = createRoot(container);
      root.render(
        <MemoryRouter initialEntries={[initialPath]}>
          <AdminPage />
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

  /**
   * Radix `TabsTrigger` elements render with `role="tab"` and surface their
   * value via `data-value` / `id="…-trigger-{value}"`. Relying on text
   * substring is fragile when OperationsTab content also contains the word
   * "Operations"; this helper targets the trigger by role + text.
   */
  function findTabTrigger(text: string) {
    return Array.from(container.querySelectorAll('[role="tab"]')).find((el) =>
      el.textContent?.includes(text),
    ) as HTMLButtonElement | undefined;
  }

  it('defaults to the access tab and fetches the token list', async () => {
    await renderPage();

    expect(adminApiMock.listTokens).toHaveBeenCalledTimes(1);
    expect(container.textContent).toContain('Ops token');
  });

  it('opens the operations tab from the URL and fetches ops + audit data', async () => {
    await renderPage('/admin?tab=operations');

    expect(adminApiMock.listTokens).not.toHaveBeenCalled();
    expect(dashboardApiMock.getLibraryState).toHaveBeenCalledWith('library-1');
    expect(adminApiMock.listAuditEvents).toHaveBeenCalled();
  });

  it('lazy-loads the pricing catalog only when the pricing tab is the URL target', async () => {
    // Access tab (default) must NOT preload the catalog.
    await renderPage();
    expect(adminApiMock.listProviders).not.toHaveBeenCalled();
    expect(adminApiMock.listModels).not.toHaveBeenCalled();

    // Unmount the access-tab instance so the catalog-loaded ref doesn't
    // survive into the pricing-tab instance and defeat the guard.
    await act(async () => {
      root?.unmount();
    });
    root = null;
    container.innerHTML = '';

    await renderPage('/admin?tab=pricing');
    // Landing directly on pricing triggers the catalog fetch exactly once
    // per mount and does NOT re-fire even though the fetched catalog is
    // empty (empty-list regression guard).
    expect(adminApiMock.listProviders).toHaveBeenCalledTimes(1);
    expect(adminApiMock.listModels).toHaveBeenCalledTimes(1);
    expect(adminApiMock.listPrices).toHaveBeenCalled();
  });

  it('opens the MCP tab from the URL and loads the canonical system prompt', async () => {
    await renderPage('/admin?tab=mcp');

    expect(queryApiMock.getAssistantSystemPrompt).toHaveBeenCalledWith('library-1');
    expect(container.textContent).toContain('MCP system prompt');
  });

  it('renders the access tab trigger and the operations tab trigger side by side', async () => {
    await renderPage();

    // Sanity check that the tab list is intact so navigating by clicking
    // stays supported even though the other tests drive via URL.
    expect(findTabTrigger('Access')).toBeTruthy();
    expect(findTabTrigger('Operations')).toBeTruthy();
    expect(findTabTrigger('Pricing')).toBeTruthy();
    expect(findTabTrigger('MCP')).toBeTruthy();
  });
});
