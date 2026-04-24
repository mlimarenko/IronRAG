import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import i18n from "@/i18n";
import { WebRunsPanel } from "@/pages/documents/WebRunsPanel";

const { documentsApiMock } = vi.hoisted(() => ({
  documentsApiMock: {
    listWebRunPages: vi.fn(),
  },
}));

vi.mock("@/api", () => ({
  documentsApi: documentsApiMock,
}));

describe("WebRunsPanel", () => {
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

  async function flushUi() {
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
  }

  async function renderPanel() {
    const runs = Array.from({ length: 12 }, (_, index) => ({
      runId: `run-${index + 1}`,
      seedUrl: `https://docs.example.com/run-${index + 1}`,
      runState: index === 0 ? "processing" : "completed",
      mode: "recursive_crawl",
      boundaryPolicy: "same_host",
      maxDepth: 3,
      maxPages: 250,
      counts: {
        discovered: 250,
        processed: index === 0 ? 120 : 250,
        failed: index === 0 ? 3 : 0,
      },
    }));

    const pages = Array.from({ length: 250 }, (_, index) => ({
      candidateId: `candidate-${index + 1}`,
      runId: "run-1",
      normalizedUrl: `https://docs.example.com/page-${String(index + 1).padStart(3, "0")}`,
      candidateState: index % 9 === 0 ? "failed" : "processed",
      depth: 2,
      httpStatus: 200,
    }));

    documentsApiMock.listWebRunPages.mockResolvedValue(pages);

    await act(async () => {
      root = createRoot(container);
      root.render(
        <WebRunsPanel
          t={i18n.t.bind(i18n)}
          webRuns={runs}
          isRefreshingRuns={false}
          onRefreshRuns={() => {}}
          onReuseRun={() => {}}
          onCancelRun={() => {}}
        />,
      );
    });

    await flushUi();
    await flushUi();
  }

  function findButton(text: string) {
    return Array.from(container.querySelectorAll("button")).find((button) =>
      button.textContent?.includes(text),
    ) as HTMLButtonElement | undefined;
  }

  it("renders runs beyond the first ten and paginates long page lists", async () => {
    await renderPanel();

    expect(container.textContent).toContain("https://docs.example.com/run-12");

    const firstRunButton = findButton("https://docs.example.com/run-1");
    expect(firstRunButton).toBeTruthy();

    await act(async () => {
      firstRunButton?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    await flushUi();
    await flushUi();

    expect(documentsApiMock.listWebRunPages).toHaveBeenCalledWith("run-1");
    expect(container.textContent).toContain("page-001");
    expect(container.textContent).toContain("1–200 of 250 URLs");
    expect(container.textContent).not.toContain("page-225");

    const nextButton = findButton("Next");
    expect(nextButton).toBeTruthy();

    await act(async () => {
      nextButton?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    await flushUi();

    expect(container.textContent).toContain("201–250 of 250 URLs");
    expect(container.textContent).toContain("page-225");
  });
});
