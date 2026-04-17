import { defineConfig, devices } from '@playwright/test';

/**
 * Self-contained Playwright config for release 0.3.1 visual QA.
 * Boots its own vite dev server (which proxies `/v1` to the real
 * backend at 127.0.0.1:19000), logs in once via `global-setup.ts`, and
 * runs each scenario against the live backend with a persisted session
 * cookie. Screenshots land under `visual-qa/screenshots/`.
 *
 * Override the backend credentials via:
 *   QA_LOGIN=admin QA_PASSWORD=... npx playwright test --config=playwright.qa.config.ts
 *
 * This config is separate from the Lovable scaffold's `playwright.config.ts`
 * so the real integration QA run is reproducible regardless of what the
 * Lovable template expects.
 */
export default defineConfig({
  testDir: './visual-qa',
  testMatch: /.*\.spec\.ts/,
  fullyParallel: false,
  workers: 1,
  retries: 0,
  reporter: [['list']],
  outputDir: 'visual-qa/.artifacts',
  globalSetup: './visual-qa/global-setup.ts',
  use: {
    baseURL: 'http://127.0.0.1:4173',
    storageState: 'visual-qa/.storage/state.json',
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
  webServer: {
    command: 'npm run dev -- --host 127.0.0.1 --port 4173 --strictPort',
    url: 'http://127.0.0.1:4173',
    reuseExistingServer: true,
    timeout: 120_000,
    stdout: 'ignore',
    stderr: 'pipe',
  },
});
