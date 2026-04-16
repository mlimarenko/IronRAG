import { chromium, type FullConfig } from '@playwright/test';

/**
 * Log in to the real backend once before the visual QA run, and persist
 * the session cookies so every scenario renders against the live API. We
 * talk through the vite dev server (which proxies `/v1` to the backend
 * at 127.0.0.1:19000) so the cookie domain matches the test's baseURL.
 *
 * Credentials are injected via `QA_LOGIN` / `QA_PASSWORD` env vars. The
 * release 0.3.0 dev default is `admin` / `rustrag123`.
 */
async function globalSetup(_config: FullConfig) {
  const login = process.env.QA_LOGIN ?? 'admin';
  const password = process.env.QA_PASSWORD ?? 'rustrag123';
  const baseURL = process.env.QA_BASE_URL ?? 'http://127.0.0.1:4173';

  const browser = await chromium.launch();
  try {
    const context = await browser.newContext({ baseURL });
    const response = await context.request.post(`${baseURL}/v1/iam/session/login`, {
      data: { login, password },
      headers: { 'content-type': 'application/json' },
    });
    if (!response.ok()) {
      throw new Error(
        `Login failed: HTTP ${response.status()} — ${await response.text()}`,
      );
    }
    // Fetch /iam/session/resolve so we have a fresh session record and the
    // backend sees one full round-trip before the scenarios run.
    const resolve = await context.request.get(`${baseURL}/v1/iam/session/resolve`);
    if (!resolve.ok()) {
      throw new Error(
        `Session resolve failed: HTTP ${resolve.status()} — ${await resolve.text()}`,
      );
    }
    await context.storageState({ path: 'visual-qa/.storage/state.json' });
    await context.close();
  } finally {
    await browser.close();
  }
}

export default globalSetup;
