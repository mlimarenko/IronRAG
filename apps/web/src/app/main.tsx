import './zodRuntime'
import { Component, type ComponentType, type ReactNode } from 'react'
import type { i18n as I18n } from 'i18next'
import { createRoot } from 'react-dom/client'
import { installSessionExpiryInterceptor } from '@/shared/api/sessionLifecycle'
import { initializeBranding } from './branding'
import '@/styles/tokens.css'
import '@/styles/layout.css'
import '@/styles/components.css'
import '@/styles/dashboard.css'
import '@/styles/documents.css'
import '@/styles/graph.css'
import '@/styles/assistant.css'
import '@/styles/admin.css'
import '@/styles/data-grid.css'
import '@/styles/misc.css'
import '@/styles/responsive.css'

// Inline ErrorBoundary so the entry chunk never has to pull in the
// observability SDK. The OpenTelemetry path lives behind a dynamic import and
// only loads the SDK when an OTLP endpoint is configured.
class RootErrorBoundary extends Component<
  { children: ReactNode; fallback: ReactNode },
  { hasError: boolean }
> {
  state = { hasError: false }
  static getDerivedStateFromError() {
    return { hasError: true }
  }
  componentDidCatch(error: unknown) {
    void import('@/shared/lib/observability').then((m) =>
      m.captureUiException(error, { feature: 'react-tree' }),
    )
  }
  render() {
    return this.state.hasError ? this.props.fallback : this.props.children
  }
}

// Fire observability lazily on idle so the SDK never bloats the main entry.
// No-op when VITE_OTEL_EXPORTER_OTLP_ENDPOINT is empty.
const idle = (cb: () => void) =>
  typeof window.requestIdleCallback === 'function'
    ? window.requestIdleCallback(cb)
    : window.setTimeout(cb, 0)
idle(() => {
  void import('@/shared/lib/observability').then((m) => m.initObservability())
})

// Register before the first request so no 401 can slip past the one place
// that turns "the server stopped accepting this session" into app state.
installSessionExpiryInterceptor()

function shouldStartBrowserMocks() {
  return (
    import.meta.env.DEV &&
    import.meta.env.VITE_ENABLE_MOCKS === 'true' &&
    new URLSearchParams(window.location.search).get('mocks') === '1'
  )
}

async function prepareRuntime() {
  if (!shouldStartBrowserMocks()) return
  const { startBrowserMocks } = await import('@/shared/api/mocks/browser')
  await startBrowserMocks()
}

function rootErrorFallback(i18n: I18n) {
  return (
    <div className="flex h-screen items-center justify-center bg-background">
      <div className="flex max-w-md flex-col gap-2 text-center">
        <h1 className="text-2xl font-semibold">{i18n.t('common.rootErrorTitle')}</h1>
        <p className="text-muted-foreground">{i18n.t('common.rootErrorDescription')}</p>
      </div>
    </div>
  )
}

function renderApp(App: ComponentType, i18n: I18n) {
  createRoot(document.getElementById('root')!).render(
    <RootErrorBoundary fallback={rootErrorFallback(i18n)}>
      <App />
    </RootErrorBoundary>,
  )
}

try {
  await prepareRuntime()
} catch {
  console.warn('Browser mock bootstrap failed; continuing without mocks')
}
async function bootstrapApp() {
  try {
    await initializeBranding()
  } catch {
    // Do not render with rejected configuration or expose raw operator input in logs.
    console.error('Frontend branding configuration failed. Check /branding.json and reload.')
    const { default: i18n } = await import('@/shared/i18n')
    createRoot(document.getElementById('root')!).render(rootErrorFallback(i18n))
    return
  }
  // App imports the deployment-derived build label; evaluate it after branding.
  const [{ default: App }, { default: i18n }] = await Promise.all([
    import('./App.tsx'),
    import('@/shared/i18n'),
  ])
  renderApp(App, i18n)
}
await bootstrapApp()
