import type { Page } from "@playwright/test";

import { iamSession, opsLibraryDashboard } from "../../../src/shared/api/mocks/fixtures";
import type { BrowserMockConfig } from "../../../src/shared/api/mocks/e2e";

export const mockPath = (path: string) => {
  const separator = path.includes("?") ? "&" : "?";
  return `${path}${separator}mocks=1`;
};

export function emptyDashboard() {
  const base = opsLibraryDashboard();

  return {
    ...base,
    attention: [],
    documentMetrics: {
      ...base.documentMetrics,
      canceled: 0,
      failed: 0,
      graphReady: 0,
      graphSparse: 0,
      processing: 0,
      queued: 0,
      ready: 0,
      total: 0,
    },
    graph: {
      ...base.graph,
      edgeCount: 0,
      edges: [],
      graphReadyDocumentCount: 0,
      graphSparseDocumentCount: 0,
      nodeCount: 0,
      nodes: [],
      readinessSummary: {
        ...base.graph.readinessSummary,
        documentCountsByReadiness: [],
        graphReadyDocumentCount: 0,
        graphSparseDocumentCount: 0,
        typedFactDocumentCount: 0,
      },
      relationCount: 0,
      status: "empty",
      typedFactDocumentCount: 0,
    },
    metrics: [],
    overview: {
      failedDocuments: 0,
      graphSparseDocuments: 0,
      processingDocuments: 0,
      readyDocuments: 0,
      totalDocuments: 0,
    },
    recentDocuments: [],
    recentWebRuns: [],
    warnings: [],
  };
}

type MockOptions = Omit<BrowserMockConfig, "session"> & {
  session?: ReturnType<typeof iamSession>;
};

export async function installBrowserMocks(
  page: Page,
  options: MockOptions = {},
) {
  const config: BrowserMockConfig = {
    authenticated: options.authenticated ?? true,
    bootstrapRequired: options.bootstrapRequired ?? false,
    dashboard: options.dashboard ?? emptyDashboard(),
    queryConversations: options.queryConversations ?? {},
    querySessions: options.querySessions ?? [],
    session: options.session ?? iamSession(),
  };

  await page.addInitScript((mockConfig) => {
    window.__IRONRAG_E2E_MOCKS__ = mockConfig;
  }, config);
}
