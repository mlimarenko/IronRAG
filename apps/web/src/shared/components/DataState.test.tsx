import { act } from "react";
import type { ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { DataState } from "./DataState";

describe("DataState", () => {
  let container: HTMLDivElement;
  let root: Root | null;

  beforeEach(() => {
    container = document.createElement("div");
    document.body.appendChild(container);
    root = createRoot(container);
  });

  afterEach(async () => {
    await act(async () => {
      root?.unmount();
    });
    container.remove();
    root = null;
  });

  async function render(ui: ReactNode) {
    await act(async () => {
      root?.render(ui);
    });
  }

  it("renders loading without invoking children", async () => {
    const children = vi.fn(() => <div>content</div>);

    await render(
      <DataState query={{ isLoading: true, error: null, data: undefined }}>
        {children}
      </DataState>,
    );

    expect(container.querySelector("[aria-label='Loading']")).toBeTruthy();
    expect(children).not.toHaveBeenCalled();
  });

  it("renders the default error alert without invoking children", async () => {
    const children = vi.fn(() => <div>content</div>);

    await render(
      <DataState query={{ isLoading: false, error: new Error("Network failed"), data: undefined }}>
        {children}
      </DataState>,
    );

    expect(container.querySelector("[role='alert']")).toBeTruthy();
    expect(container.textContent).toContain("The request failed.");
    expect(container.textContent).not.toContain("Network failed");
    expect(children).not.toHaveBeenCalled();
  });

  it("does not render raw backend error strings by default", async () => {
    const children = vi.fn(() => <div>content</div>);

    await render(
      <DataState query={{ isLoading: false, error: "provider returned internal stack trace", data: undefined }}>
        {children}
      </DataState>,
    );

    expect(container.textContent).toContain("The request failed.");
    expect(container.textContent).not.toContain("provider returned internal stack trace");
    expect(children).not.toHaveBeenCalled();
  });

  it("renders empty content when the empty predicate matches", async () => {
    const children = vi.fn(() => <div>content</div>);

    await render(
      <DataState
        query={{ isLoading: false, error: null, data: [] as string[] }}
        emptyCheck={(items) => items.length === 0}
        emptyRender={<div>No rows</div>}
      >
        {children}
      </DataState>,
    );

    expect(container.textContent).toContain("No rows");
    expect(children).not.toHaveBeenCalled();
  });

  it("invokes children with non-undefined data", async () => {
    const children = vi.fn((data: { name: string }) => <div>{data.name}</div>);

    await render(
      <DataState query={{ isLoading: false, error: null, data: { name: "Alpha Suite" } }}>
        {children}
      </DataState>,
    );

    expect(container.textContent).toContain("Alpha Suite");
    expect(children).toHaveBeenCalledWith({ name: "Alpha Suite" });
  });
});
