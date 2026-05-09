// MSW worker for browser-side mocking (Storybook, dev experiments, e2e smoke).
// Not imported by the production app entry unless VITE_ENABLE_MOCKS is enabled
// and the current URL opts into browser mocks.

import { setupWorker } from "msw/browser";
import { handlers } from "./handlers";
import { createBrowserMockHandlers } from "./e2e";

const worker = setupWorker(...handlers);

export async function startBrowserMocks() {
  worker.use(...createBrowserMockHandlers(window.__IRONRAG_E2E_MOCKS__));
  await worker.start({
    serviceWorker: {
      url: "/mockServiceWorker.js",
    },
    onUnhandledRequest(request, print) {
      const { pathname } = new URL(request.url);
      if (pathname.startsWith("/v1/")) {
        print.error();
      }
    },
  });
  window.__IRONRAG_MSW_READY__ = true;
}
