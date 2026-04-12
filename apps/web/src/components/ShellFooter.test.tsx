import { act } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { ShellFooter } from "@/components/ShellFooter";
import { versionApi } from "@/api/version";

vi.mock("@/api/version", () => ({
  versionApi: {
    getReleaseUpdate: vi.fn(),
  },
}));

async function flushUi() {
  await act(async () => {
    await new Promise(resolve => setTimeout(resolve, 0));
  });
}

describe("ShellFooter", () => {
  let container: HTMLDivElement;
  let root: Root | null;

  beforeEach(() => {
    vi.clearAllMocks();
    container = document.createElement("div");
    document.body.appendChild(container);
    root = null;
  });

  afterEach(async () => {
    if (root) {
      await act(async () => {
        root?.unmount();
      });
    }
    container.remove();
  });

  async function renderFooter() {
    const queryClient = new QueryClient({
      defaultOptions: {
        queries: {
          retry: false,
        },
      },
    });

    await act(async () => {
      root = createRoot(container);
      root.render(
        <QueryClientProvider client={queryClient}>
          <ShellFooter />
        </QueryClientProvider>,
      );
    });

    await flushUi();
    await flushUi();
  }

  it("shows update link when a newer release is available", async () => {
    vi.mocked(versionApi.getReleaseUpdate).mockResolvedValue({
      status: "update_available",
      currentVersion: "0.1.2",
      latestVersion: "0.1.3",
      releaseUrl: "https://github.com/mlimarenko/IronRAG/releases/tag/v0.1.3",
      repositoryUrl: "https://github.com/mlimarenko/IronRAG",
      checkedAt: "2026-04-08T18:00:00Z",
    });

    await renderFooter();

    expect(container.textContent).toContain("IronRAG v0.1.2");
    const updateLink = Array.from(container.querySelectorAll("a")).find(node =>
      node.textContent?.includes("Update available: v0.1.3"),
    );
    expect(updateLink?.getAttribute("href")).toBe(
      "https://github.com/mlimarenko/IronRAG/releases/tag/v0.1.3",
    );
  });

  it("stays quiet when the current build is already latest", async () => {
    vi.mocked(versionApi.getReleaseUpdate).mockResolvedValue({
      status: "up_to_date",
      currentVersion: "0.1.2",
      latestVersion: "0.1.2",
      releaseUrl: "https://github.com/mlimarenko/IronRAG/releases/tag/v0.1.2",
      repositoryUrl: "https://github.com/mlimarenko/IronRAG",
      checkedAt: "2026-04-08T18:00:00Z",
    });

    await renderFooter();

    expect(versionApi.getReleaseUpdate).toHaveBeenCalledTimes(1);
    expect(container.textContent).not.toContain("Update available: v0.1.2");
  });
});
