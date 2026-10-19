import { defineConfig, devices } from '@playwright/test'

const qaBaseURL = process.env.QA_BASE_URL ?? 'http://127.0.0.1:4173'
const qaStorageStatePath = process.env.QA_STORAGE_STATE_PATH ?? 'visual-qa/.storage/state.json'
const qaUsesExternalServer = process.env.QA_EXTERNAL === '1'

/**
 * Self-contained Playwright config for release visual QA. By default it boots
 * a Vite dev server (which proxies `/v1` to the configured backend), logs in
 * once via `global-setup.ts`, and persists the session for every scenario.
 * Set `QA_EXTERNAL=1` together with `QA_BASE_URL` to inspect an already
 * deployed frontend without trying to start Vite on that external address.
 * Screenshots land under `visual-qa/screenshots/`.
 *
 * Override the backend credentials via:
 *   QA_LOGIN=admin QA_PASSWORD=... npx playwright test --config=playwright.qa.config.ts
 *   QA_EXTERNAL=1 QA_BASE_URL=https://example.test ...
 *
 * This config is separate from the Lovable scaffold's `playwright.config.ts`
 * so the real integration QA run is reproducible regardless of what the
 * Lovable template expects.
 */
export default defineConfig({
  testDir: './visual-qa',
  testMatch: '**/*.spec.ts',
  fullyParallel: false,
  workers: 1,
  retries: 0,
  reporter: [['list']],
  outputDir: 'visual-qa/.artifacts',
  globalSetup: './visual-qa/global-setup.ts',
  use: {
    baseURL: qaBaseURL,
    storageState: qaStorageStatePath,
    trace: 'off',
    screenshot: 'off',
    video: 'off',
    launchOptions: {
      args: ['--no-sandbox', '--disable-dev-shm-usage'],
    },
  },
  projects: [
    {
      name: 'desktop',
      use: {
        ...devices['Desktop Chrome'],
        viewport: { width: 1440, height: 900 },
      },
    },
    {
      // Mobile viewport on Chromium — sandbox does not ship WebKit. Device
      // emulation through Chromium matches the viewport + UA for layout
      // testing purposes.
      name: 'mobile',
      use: {
        ...devices['Pixel 7'],
      },
    },
  ],
  webServer: qaUsesExternalServer
    ? undefined
    : {
        command: 'npm run dev -- --host 127.0.0.1 --port 4173 --strictPort',
        url: qaBaseURL,
        reuseExistingServer: true,
        timeout: 120_000,
        stdout: 'ignore',
        stderr: 'pipe',
      },
})
