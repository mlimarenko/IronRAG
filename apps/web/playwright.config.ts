import { defineConfig, devices } from '@playwright/test'

const storybookVisualPathPattern = /(^|[\\/])tests[\\/]visual([\\/]|$)/
const hasStorybookVisualArg = process.argv
  .slice(2)
  .some((arg) => storybookVisualPathPattern.test(arg))
if (hasStorybookVisualArg) {
  process.env.PLAYWRIGHT_STORYBOOK_VISUAL = '1'
}

const isStorybookVisualRun = process.env.PLAYWRIGHT_STORYBOOK_VISUAL === '1'

export default defineConfig({
  testDir: isStorybookVisualRun ? 'tests/visual' : 'tests/e2e',
  workers: 1,
  reporter: [['list'], ['html', { outputFolder: 'playwright-report' }]],
  ...(isStorybookVisualRun ? { snapshotPathTemplate: '{testDir}/__screenshots__/{arg}{ext}' } : {}),
  use: {
    baseURL: isStorybookVisualRun ? 'http://localhost:6006' : 'http://127.0.0.1:3000',
    trace: 'retain-on-failure',
  },
  projects: [
    {
      name: 'chromium',
      use: {
        ...devices['Desktop Chrome'],
        ...(isStorybookVisualRun ? { viewport: { width: 1280, height: 800 } } : {}),
      },
    },
  ],
  webServer: isStorybookVisualRun
    ? {
        command:
          'npx vite preview --host localhost --port 6006 --strictPort --outDir storybook-static',
        port: 6006,
        reuseExistingServer: !process.env.CI,
      }
    : {
        command: 'npm run dev',
        port: 3000,
        reuseExistingServer: !process.env.CI,
        env: {
          VITE_ENABLE_MOCKS: 'true',
        },
      },
})
