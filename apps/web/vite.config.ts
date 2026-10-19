import { readFileSync } from 'node:fs'
import path from 'node:path'
import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'

const packageJson = JSON.parse(readFileSync(path.resolve(__dirname, 'package.json'), 'utf8')) as {
  version?: string
}

const normalizeBuildVersion = (value?: string) => {
  const trimmed = value?.trim()
  if (!trimmed) {
    return packageJson.version ?? '0.0.0'
  }
  return trimmed.replace(/^v(?=\d)/, '')
}

const appVersion = normalizeBuildVersion(process.env.APP_VERSION ?? process.env.VITE_APP_VERSION)

const vendorChunk = (id: string) => {
  const normalizedId = id.replaceAll('\\', '/')
  if (!normalizedId.includes('/node_modules/')) {
    return null
  }

  if (
    normalizedId.includes('/node_modules/react/') ||
    normalizedId.includes('/node_modules/react-dom/') ||
    normalizedId.includes('/node_modules/react-router') ||
    normalizedId.includes('/node_modules/scheduler/')
  ) {
    return 'vendor-react'
  }

  if (normalizedId.includes('/node_modules/@tanstack/')) {
    return 'vendor-query'
  }

  // Radix primitives back the shell, dialogs, selects and dropdowns across
  // both eager (AppShell) and lazy (admin/documents/assistant) surfaces.
  // Pinning them to one chunk keeps a single cached copy instead of letting
  // the splitter inline Radix into whichever feature chunk touches it first.
  if (normalizedId.includes('/node_modules/@radix-ui/')) {
    return 'vendor-radix'
  }

  // zod is reached eagerly: App.tsx imports LoginPage statically, and the
  // bootstrap form validates with zod. Splitting it into its own chunk keeps
  // validation weight out of index-*.js, which carries a 150 KB gzip ceiling
  // enforced by scripts/check-bundle-budget.mjs and the size-limit config.
  if (normalizedId.includes('/node_modules/zod/')) {
    return 'vendor-zod'
  }

  if (
    normalizedId.includes('/node_modules/i18next/') ||
    normalizedId.includes('/node_modules/react-i18next/')
  ) {
    return 'vendor-i18n'
  }

  if (normalizedId.includes('/node_modules/@opentelemetry/')) {
    return 'vendor-observability'
  }

  return null
}

export default defineConfig({
  define: {
    __APP_VERSION__: JSON.stringify(appVersion),
  },
  server: {
    host: '::',
    port: 3000,
    proxy: {
      // Dev-only: point the proxy at another IronRAG backend (e.g. a stage
      // host) with IRONRAG_DEV_API_TARGET when reviewing against real data.
      '/v1': {
        target: process.env.IRONRAG_DEV_API_TARGET ?? 'http://127.0.0.1:19000',
        changeOrigin: true,
        secure: false,
      },
    },
  },
  build: {
    outDir: 'dist',
    sourcemap: true,
    // Vendor chunks are isolated and lazy-loaded; the enforced gzip budgets
    // live in package.json and catch real regressions.
    chunkSizeWarningLimit: 600,
    rolldownOptions: {
      output: {
        codeSplitting: {
          includeDependenciesRecursively: true,
          groups: [
            {
              name: vendorChunk,
              test: /node_modules/,
            },
          ],
        },
      },
    },
  },
  plugins: [tailwindcss(), react()],
  resolve: {
    alias: {
      '@': path.resolve(__dirname, './src'),
    },
    dedupe: [
      'react',
      'react-dom',
      'react/jsx-runtime',
      'react/jsx-dev-runtime',
      '@tanstack/react-query',
      '@tanstack/query-core',
    ],
  },
})
