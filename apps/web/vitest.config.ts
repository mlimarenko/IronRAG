import { defineConfig } from 'vitest/config'
import react from '@vitejs/plugin-react'
import path from 'node:path'
import { readFileSync } from 'node:fs'

const packageJson = JSON.parse(readFileSync(path.resolve(__dirname, 'package.json'), 'utf8')) as {
  version?: string
}

export default defineConfig({
  define: {
    __APP_VERSION__: JSON.stringify(packageJson.version ?? '0.0.0'),
  },
  plugins: [react()],
  test: {
    environment: 'jsdom',
    globals: true,
    setupFiles: ['./src/shared/test/setup.ts'],
    include: ['src/**/*.{test,spec}.{ts,tsx}'],
    coverage: {
      provider: 'v8',
      reporter: [
        'text',
        'json-summary',
        'html',
        ['lcov', { projectRoot: path.resolve(__dirname, '../..') }],
      ],
      reportsDirectory: 'coverage',
      exclude: [
        'src/shared/api/generated/**',
        'src/shared/api/mocks/handlers.ts',
        '**/*.stories.tsx',
        '**/*.test.tsx',
        'tests/e2e/**',
      ],
      thresholds: {
        // Ratchet against the measured 2026-07 quality baseline. Keep a small
        // instrumentation margin, but do not allow broad coverage erosion.
        lines: 66.5,
        functions: 55.5,
        statements: 63.5,
        branches: 54.8,
      },
    },
  },
  resolve: {
    alias: { '@': path.resolve(__dirname, './src') },
  },
})
