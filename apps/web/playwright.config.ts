import { defineConfig, devices } from '@playwright/test'

const port = Number(process.env.PLAYWRIGHT_PORT ?? 4174)
if (!Number.isInteger(port) || port < 1 || port > 65535) {
  throw new Error('PLAYWRIGHT_PORT must be an integer from 1 to 65535')
}
const baseURL = `http://127.0.0.1:${port}`
const isCi = Boolean(process.env.CI)

export default defineConfig({
  testDir: './tests/e2e',
  testMatch: '**/*.spec.ts',
  testIgnore: ['**/*.test.ts', '**/*.test.tsx'],
  fullyParallel: false,
  forbidOnly: isCi,
  retries: isCi ? 2 : 0,
  workers: 2,
  // The JSON report is what the spec gate reads: a run that collected nothing
  // still exits 0, so the report, not the exit code, proves the suite ran.
  reporter: [isCi ? ['line'] : ['list'], ['json', { outputFile: 'test-results/e2e-report.json' }]],
  outputDir: 'test-results/e2e',
  timeout: 30_000,
  expect: {
    timeout: 5_000,
  },
  use: {
    ...devices['Desktop Chrome'],
    baseURL,
    trace: 'retain-on-failure',
    screenshot: 'only-on-failure',
    video: 'off',
    launchOptions: {
      args: ['--no-sandbox', '--disable-dev-shm-usage'],
    },
  },
  projects: [
    {
      name: 'chromium',
      use: {
        ...devices['Desktop Chrome'],
      },
    },
  ],
  webServer: {
    command: `npm run dev -- --host 127.0.0.1 --port ${port} --strictPort`,
    url: baseURL,
    env: {
      VITE_ENABLE_MOCKS: 'true',
    },
    reuseExistingServer: !isCi,
    timeout: 120_000,
    stdout: 'ignore',
    stderr: 'pipe',
  },
})
