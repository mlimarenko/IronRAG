import { act } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import AdminPage from "@/features/admin/AdminPage";
import { TooltipProvider } from "@/shared/components/ui/tooltip";

/**
 * Admin integration tests for the §3.4 nested-route restructure. The flat
 * `?tab=` AdminPage was dissolved into nested routes under `/admin`; these
 * tests assert the router resolves each section, the redirect works, and the
 * role gates (ai/users/system) behave. Per-section mutation behaviour lives in
 * the section components' own suites — here we validate the routing shell.
 */

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
    listUsers: vi.fn(),
    createUser: vi.fn(),
    setUserRole: vi.fn(),
    listWorkspaces: vi.fn(),
    listLibraries: vi.fn(),
    listProviders: vi.fn(),
    listModels: vi.fn(),
    listCredentials: vi.fn(),
    listPresets: vi.fn(),
    listBindings: vi.fn(),
    listPrices: vi.fn(),
    listAuditEvents: vi.fn(),
    listIngestQueue: vi.fn(),
    listIngestStageEvents: vi.fn(),
    updateLibraryMcpSettings: vi.fn(),
  },
  dashboardApiMock: { getLibraryState: vi.fn() },
  librarySnapshotApiMock: { downloadExport: vi.fn() },
  queryApiMock: { getAssistantSystemPrompt: vi.fn() },
}));

vi.mock("@/shared/contexts/app-context", () => ({
  useApp: () => useAppMock(),
}));

// AdminSystemPage reads theme prefs; provide a non-throwing stub so the System
// route can mount in isolation without the real PreferencesProvider.
vi.mock("@/shared/contexts/preferences-context", () => ({
  usePreferences: () => ({
    theme: "system",
    resolvedTheme: "light",
    setTheme: vi.fn(),
    cycleTheme: vi.fn(),
    developerMode: false,
    setDeveloperMode: vi.fn(),
    toggleDeveloperMode: vi.fn(),
  }),
  useDeveloperMode: () => false,
}));

vi.mock("sonner", () => ({
  toast: { error: vi.fn(), success: vi.fn(), warning: vi.fn(), loading: vi.fn(() => "t") },
}));

vi.mock("@/shared/api", () => ({
  adminApi: adminApiMock,
  dashboardApi: dashboardApiMock,
  librarySnapshotApi: librarySnapshotApiMock,
  queryApi: queryApiMock,
  // LibrariesTab imports Catalog/Ops/unwrap from the api barrel; provide them
  // so the catalog renders without throwing/looping in the router tests.
  Catalog: { deleteCatalogLibrary: vi.fn() },
  Ops: { getAsyncOperation: vi.fn() },
  unwrap: (value: { data?: unknown }) => value?.data ?? value,
  adminModelCatalogOptions: (params: Record<string, unknown> = {}) => ({
    queryKey: ["modelCatalog", params],
    queryFn: async () => adminApiMock.listModels(params),
  }),
  ASYNC_OPERATION_TERMINAL_STATES: new Set(["ready", "failed", "canceled", "superseded"]),
  queries: {
    getAssistantSystemPromptOptions: (input?: { query?: { libraryId?: string } }) => ({
      queryKey: ["sysPrompt", input?.query?.libraryId ?? null],
      queryFn: async () => queryApiMock.getAssistantSystemPrompt(input?.query?.libraryId),
    }),
    getLibraryStateOptions: (input: { path: { libraryId: string } }) => ({
      queryKey: ["libState", input.path.libraryId],
      queryFn: async () => dashboardApiMock.getLibraryState(input.path.libraryId),
    }),
    getCatalogLibraryOptions: (input: { path: { libraryId: string } }) => ({
      queryKey: ["catalogLibrary", input.path.libraryId],
      queryFn: async () => ({
        id: input.path.libraryId,
        workspaceId: "ws-1",
        slug: "library-1",
        displayName: "Library 1",
        lifecycleState: "active",
        includeDocumentHintInMcpAnswers: true,
        ingestionReadiness: { ready: true, missingBindingPurposes: [] },
        recognitionPolicy: {},
        webIngestPolicy: {},
      }),
    }),
    listAuditEventsOptions: (input?: { query?: unknown }) => ({
      queryKey: ["audit", input?.query ?? null],
      queryFn: async () => adminApiMock.listAuditEvents(input?.query ?? {}),
    }),
    listIngestQueueOptions: () => ({
      queryKey: ["queue"],
      queryFn: async () => adminApiMock.listIngestQueue(),
    }),
    listIngestQueueQueryKey: () => ["queue"],
    listIngestStageEventsOptions: (input: { path: { attemptId: string } }) => ({
      queryKey: ["ingestStageEvents", input.path.attemptId],
      queryFn: async () => adminApiMock.listIngestStageEvents(input.path.attemptId),
    }),
    listIamTokensOptions: () => ({
      queryKey: ["tokens"],
      queryFn: async () => adminApiMock.listTokens(),
    }),
    listIamUsersOptions: () => ({
      queryKey: ["users"],
      queryFn: async () => adminApiMock.listUsers(),
    }),
    listCatalogWorkspacesOptions: () => ({
      queryKey: ["workspaces"],
      queryFn: async () => adminApiMock.listWorkspaces(),
    }),
    listCatalogLibrariesOptions: (input: { path: { workspaceId: string } }) => ({
      queryKey: ["libraries", input.path.workspaceId],
      queryFn: async () => adminApiMock.listLibraries(input.path.workspaceId),
    }),
    listCatalogWorkspacesQueryKey: () => ["workspaces"],
    listCatalogLibrariesQueryKey: (input: { path: { workspaceId: string } }) => [
      "libraries",
      input.path.workspaceId,
    ],
    getWorkspaceCostSummaryOptions: (input: { query: { workspaceId: string } }) => ({
      queryKey: ["wsCost", input.query.workspaceId],
      queryFn: async () => ({
        totalCost: "1.25",
        currencyCode: "USD",
        libraryCount: 1,
        documentCount: 7,
        providerCallCount: 11,
      }),
    }),
    getWorkspaceCostSummaryQueryKey: (input: { query: { workspaceId: string } }) => [
      "wsCost",
      input.query.workspaceId,
    ],
    getLibraryCostSummaryOptions: (input: { query: { libraryId: string } }) => ({
      queryKey: ["libCost", input.query.libraryId],
      queryFn: async () => ({
        totalCost: "0.50",
        currencyCode: "USD",
        documentCount: 3,
        providerCallCount: 5,
      }),
    }),
    getLibraryCostSummaryQueryKey: (input: { query: { libraryId: string } }) => [
      "libCost",
      input.query.libraryId,
    ],
  },
}));

// Heavy panels are not what these routing tests validate.
vi.mock("@/features/admin/components/AiConfigurationPanel", () => ({
  default: () => <div data-testid="ai-panel">AI panel</div>,
}));

const ADMIN_USER = {
  user: { id: "u-1", login: "admin", displayName: "Admin", accessLabel: "Admin", role: "admin" as const },
  activeWorkspace: { id: "ws-1", name: "Workspace 1" },
  activeLibrary: { id: "library-1", name: "Library 1" },
  libraries: [
    {
      id: "library-1",
      workspaceId: "ws-1",
      name: "Library 1",
      createdAt: "2026-05-14T00:00:00Z",
      includeDocumentHintInMcpAnswers: false,
      ingestionReady: true,
      queryReady: true,
      missingBindingPurposes: [],
    },
  ],
  workspaces: [{ id: "ws-1", name: "Workspace 1", createdAt: "2026-05-14T00:00:00Z" }],
  setActiveWorkspace: vi.fn(),
  setActiveLibrary: vi.fn(),
  setLibraries: vi.fn(),
  selectWorkspaceLibrary: vi.fn(() => true),
  refreshSession: vi.fn(),
  locale: "en",
  setLocale: vi.fn(),
};

describe("AdminPage routing", () => {
  let container: HTMLDivElement;
  let root: Root | null;

  beforeEach(() => {
    vi.clearAllMocks();
    Element.prototype.scrollIntoView = vi.fn();
    Element.prototype.hasPointerCapture = vi.fn(() => false);
    window.localStorage.clear();
    container = document.createElement("div");
    document.body.appendChild(container);
    root = null;

    useAppMock.mockReturnValue(ADMIN_USER);
    adminApiMock.listTokens.mockResolvedValue([]);
    adminApiMock.listUsers.mockResolvedValue([
      {
        principalId: "u-1",
        login: "admin",
        email: "admin@example.com",
        displayName: "Admin",
        role: "admin",
        authProviderKind: "password",
        externalSubject: null,
      },
    ]);
    adminApiMock.listWorkspaces.mockResolvedValue([
      { id: "ws-1", slug: "workspace-1", displayName: "Workspace 1", lifecycleState: "active" },
    ]);
    adminApiMock.listLibraries.mockResolvedValue([
      {
        id: "library-1",
        workspaceId: "ws-1",
        slug: "library-1",
        displayName: "Library 1",
        lifecycleState: "active",
        includeDocumentHintInMcpAnswers: false,
        ingestionReadiness: { ready: true, missingBindingPurposes: [] },
        recognitionPolicy: {},
        webIngestPolicy: {},
      },
    ]);
    adminApiMock.listAuditEvents.mockResolvedValue({ items: [], total: 0, limit: 50, offset: 0 });
    adminApiMock.listIngestQueue.mockResolvedValue({
      summary: { running: 0, queued: 0, paused: 0, total: 0 },
      items: [],
    });
    adminApiMock.listIngestStageEvents.mockResolvedValue({ stages: [] });
    dashboardApiMock.getLibraryState.mockResolvedValue({
      state: {
        queueDepth: 0,
        runningAttempts: 0,
        readableDocumentCount: 0,
        failedDocumentCount: 0,
        degradedState: "healthy",
        knowledgeGenerationState: "graph_ready",
        lastRecomputedAt: "2026-04-10T10:00:00Z",
      },
      warnings: [],
    });
    queryApiMock.getAssistantSystemPrompt.mockResolvedValue({ rendered: "# MCP prompt", template: "# t" });
  });

  afterEach(async () => {
    if (root) await act(async () => root?.unmount());
    container.remove();
  });

  async function flush() {
    await act(async () => {
      await new Promise((r) => setTimeout(r, 0));
    });
  }

  async function renderAt(initialPath: string) {
    const queryClient = new QueryClient({
      defaultOptions: { queries: { retry: false, staleTime: 0, refetchOnWindowFocus: false } },
    });
    await act(async () => {
      root = createRoot(container);
      root.render(
        <QueryClientProvider client={queryClient}>
          <TooltipProvider>
            <MemoryRouter initialEntries={[initialPath]}>
              <Routes>
                <Route path="/admin/*" element={<AdminPage />} />
              </Routes>
            </MemoryRouter>
          </TooltipProvider>
        </QueryClientProvider>,
      );
    });
    await flush();
    await flush();
  }

  it("redirects /admin to the libraries catalog", async () => {
    await renderAt("/admin");
    expect(adminApiMock.listWorkspaces).toHaveBeenCalled();
    expect(container.textContent).toContain("Library 1");
  });

  it("renders the libraries catalog at /admin/libraries", async () => {
    await renderAt("/admin/libraries");
    expect(container.textContent).toContain("Library 1");
    expect(container.textContent).toContain("Total cost");
  });

  it("renders the Library Hub at /admin/library/:id with section nav", async () => {
    await renderAt("/admin/library/library-1");
    expect(container.textContent).toContain("Library 1");
    // Section switcher + Configure AI deep-link present.
    expect(container.textContent).toContain("Configure AI");
    expect(container.textContent).toContain("Overview");
    expect(container.textContent).toContain("Backup");
  });

  it("keeps the Library Hub activity focused on per-library health, not the global queue", async () => {
    await renderAt("/admin/library/library-1?section=activity");
    expect(dashboardApiMock.getLibraryState).toHaveBeenCalled();
    expect(adminApiMock.listIngestQueue).not.toHaveBeenCalled();
    expect(container.textContent).not.toContain("Ingest Queue");
  });

  it("keeps the MCP source hint toggle checked after a successful save", async () => {
    adminApiMock.updateLibraryMcpSettings.mockResolvedValue({
      id: "library-1",
      workspaceId: "ws-1",
      slug: "library-1",
      displayName: "Library 1",
      lifecycleState: "active",
      includeDocumentHintInMcpAnswers: true,
      ingestionReadiness: { ready: true, missingBindingPurposes: [] },
      recognitionPolicy: {},
      webIngestPolicy: {},
    });

    await renderAt("/admin/library/library-1?section=mcp");

    const checkbox = container.querySelector<HTMLElement>('[role="checkbox"]');
    expect(checkbox).toBeTruthy();
    expect(checkbox?.getAttribute("aria-checked")).toBe("false");

    await act(async () => {
      checkbox?.click();
    });
    await flush();

    expect(adminApiMock.updateLibraryMcpSettings).toHaveBeenCalledWith("library-1", {
      includeDocumentHintInMcpAnswers: true,
    });
    expect(checkbox?.getAttribute("aria-checked")).toBe("true");
    expect(ADMIN_USER.setLibraries).toHaveBeenCalled();
    expect(ADMIN_USER.setActiveLibrary).toHaveBeenCalledWith(
      expect.objectContaining({ includeDocumentHintInMcpAnswers: true }),
    );
    expect(ADMIN_USER.refreshSession).toHaveBeenCalled();
  });

  it("enables the MCP source hint toggle from the routed library payload", async () => {
    useAppMock.mockReturnValue({
      ...ADMIN_USER,
      activeLibrary: null,
      libraries: [],
    });

    await renderAt("/admin/library/library-1?section=mcp");
    await flush();

    const checkbox = container.querySelector<HTMLElement>('[role="checkbox"]');
    expect(checkbox).toBeTruthy();
    expect(checkbox?.getAttribute("aria-checked")).toBe("true");
    expect(checkbox?.hasAttribute("disabled")).toBe(false);
    expect(checkbox?.getAttribute("data-disabled")).toBeNull();
  });

  it("renders the AI configuration section at /admin/ai", async () => {
    await renderAt("/admin/ai");
    expect(container.querySelector('[data-testid="ai-panel"]')).toBeTruthy();
  });

  it("renders the access (tokens) section at /admin/access", async () => {
    await renderAt("/admin/access");
    expect(adminApiMock.listTokens).toHaveBeenCalled();
  });

  it("renders the global ingest queue section at /admin/queue", async () => {
    await renderAt("/admin/queue");
    expect(adminApiMock.listIngestQueue).toHaveBeenCalled();
    expect(container.textContent).toContain("Global ingest queue");
  });

  it("renders the users surface at /admin/users for an admin", async () => {
    await renderAt("/admin/users");
    expect(adminApiMock.listUsers).toHaveBeenCalled();
    expect(container.textContent).toContain("Create user");
    // The seeded admin row renders by email.
    expect(container.textContent).toContain("admin@example.com");
  });

  it("renders the system settings section at /admin/system", async () => {
    await renderAt("/admin/system");
    expect(container.textContent).toContain("System settings");
    expect(container.textContent).toContain("API");
  });

  it("falls back to the catalog for an unknown admin sub-route", async () => {
    await renderAt("/admin/does-not-exist");
    expect(container.textContent).toContain("Library 1");
  });

  it("hides role-gated sections from a non-admin and redirects them to the catalog", async () => {
    useAppMock.mockReturnValue({
      ...ADMIN_USER,
      user: { ...ADMIN_USER.user, role: "operator" as const },
    });
    await renderAt("/admin/users");
    // Operators lack users.manage → the route is not registered → redirect.
    expect(adminApiMock.listUsers).not.toHaveBeenCalled();
    expect(container.textContent).toContain("Library 1");
  });

  it("hides the ingest queue section from a non-admin", async () => {
    useAppMock.mockReturnValue({
      ...ADMIN_USER,
      user: { ...ADMIN_USER.user, role: "operator" as const },
    });
    await renderAt("/admin/queue");
    expect(adminApiMock.listIngestQueue).not.toHaveBeenCalled();
    expect(container.textContent).toContain("Library 1");
  });
});
