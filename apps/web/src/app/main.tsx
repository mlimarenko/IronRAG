import './zodRuntime'
import { Component, type ReactNode } from 'react'
import { createRoot } from 'react-dom/client'
import i18n from '@/shared/i18n'
import App from './App.tsx'
import '../index.css'

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

function renderApp() {
  createRoot(document.getElementById('root')!).render(
    <RootErrorBoundary
      fallback={
        <div className="flex h-screen items-center justify-center bg-background">
          <div className="max-w-md space-y-2 text-center">
            <h1 className="text-2xl font-semibold">{i18n.t('common.rootErrorTitle')}</h1>
            <p className="text-muted-foreground">{i18n.t('common.rootErrorDescription')}</p>
          </div>
        </div>
      }
    >
      <App />
    </RootErrorBoundary>,
  )
}

void prepareRuntime().then(renderApp)
