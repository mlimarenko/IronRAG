import "@testing-library/jest-dom";
import { afterAll, afterEach, beforeAll } from "vitest";
import { server } from "../api/mocks/server";

const createTestStorage = (): Storage => {
  const items = new Map<string, string>();

  return {
    get length() {
      return items.size;
    },
    clear: () => items.clear(),
    getItem: (key: string) => items.get(key) ?? null,
    key: (index: number) => Array.from(items.keys())[index] ?? null,
    removeItem: (key: string) => {
      items.delete(key);
    },
    setItem: (key: string, value: string) => {
      items.set(key, value);
    },
  };
};

const testStorage = window.localStorage ?? createTestStorage();

Object.defineProperty(window, "localStorage", {
  configurable: true,
  value: testStorage,
});

Object.defineProperty(globalThis, "localStorage", {
  configurable: true,
  value: testStorage,
});

await import("../i18n");

// Sprint 5: MSW lifecycle. `bypass` for unhandled requests is critical so
// existing `vi.mock('@/shared/api')` and `vi.spyOn(Tag, 'method')` overrides keep
// winning — MSW only intercepts requests that actually hit the network.
beforeAll(() => server.listen({ onUnhandledRequest: "bypass" }));
afterEach(() => server.resetHandlers());
afterAll(() => server.close());

Object.defineProperty(window, "matchMedia", {
  writable: true,
  value: (query: string) => ({
    matches: false,
    media: query,
    onchange: null,
    addListener: () => {},
    removeListener: () => {},
    addEventListener: () => {},
    removeEventListener: () => {},
    dispatchEvent: () => {},
  }),
});
