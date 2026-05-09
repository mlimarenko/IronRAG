import "@testing-library/jest-dom";
import "../i18n";
import { afterAll, afterEach, beforeAll } from "vitest";
import { server } from "../api/mocks/server";

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
