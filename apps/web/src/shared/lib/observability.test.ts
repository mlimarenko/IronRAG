import { afterEach, describe, expect, it, vi } from 'vitest'

import { TELEMETRY_CONSENT_STORAGE_KEY } from './telemetry/consent'

type Handler = (metric: { value: number; rating: string }) => void

const otelModules = [
  '@opentelemetry/api',
  '@opentelemetry/api-logs',
  '@opentelemetry/sdk-trace-web',
  '@opentelemetry/exporter-trace-otlp-http',
  '@opentelemetry/resources',
  '@opentelemetry/semantic-conventions',
  '@opentelemetry/semantic-conventions/incubating',
  '@opentelemetry/instrumentation',
  '@opentelemetry/instrumentation-fetch',
  '@opentelemetry/instrumentation-document-load',
  '@opentelemetry/instrumentation-user-interaction',
  '@opentelemetry/sdk-metrics',
  '@opentelemetry/exporter-metrics-otlp-http',
  '@opentelemetry/sdk-logs',
  '@opentelemetry/exporter-logs-otlp-http',
  '@opentelemetry/context-zone-peer-dep',
  'web-vitals',
]

function mockSdk() {
  const counters = new Map<string, { add: ReturnType<typeof vi.fn> }>()
  const histograms = new Map<string, { record: ReturnType<typeof vi.fn> }>()
  const loggerEmit = vi.fn()
  const traceExporterInstance = {
    export: vi.fn(),
    forceFlush: vi.fn().mockResolvedValue(undefined),
    shutdown: vi.fn().mockResolvedValue(undefined),
  }
  const otlpTraceExporter = vi.fn(function () {
    return traceExporterInstance
  })
  const otlpMetricExporter = vi.fn()
  const otlpLogExporter = vi.fn()
  const batchSpanExporter: { current: { export: (...args: unknown[]) => void } | null } = {
    current: null,
  }
  const registerInstrumentations = vi.fn()
  const resourceFromAttributes = vi.fn((attrs: Record<string, unknown>) => attrs)
  const setGlobalMeterProvider = vi.fn()
  const setGlobalLoggerProvider = vi.fn()
  const disableTrace = vi.fn()
  const disableMetrics = vi.fn()
  const disableLogs = vi.fn()
  const register = vi.fn()
  const forceFlush = vi.fn().mockResolvedValue(undefined)
  const shutdown = vi.fn().mockResolvedValue(undefined)
  const span = {
    recordException: vi.fn(),
    setAttributes: vi.fn(),
    setStatus: vi.fn(),
    end: vi.fn(),
  }
  const tracer = { startSpan: vi.fn(() => span) }
  const meter = {
    createCounter: vi.fn((name: string) => {
      const counter = { add: vi.fn() }
      counters.set(name, counter)
      return counter
    }),
    createHistogram: vi.fn((name: string) => {
      const histogram = { record: vi.fn() }
      histograms.set(name, histogram)
      return histogram
    }),
  }
  const vitals: Partial<Record<'cls' | 'fcp' | 'inp' | 'lcp' | 'ttfb', Handler>> = {}

  vi.doMock('@opentelemetry/api', () => ({
    SpanStatusCode: { ERROR: 2 },
    trace: { getTracer: vi.fn(() => tracer), disable: disableTrace },
    metrics: { setGlobalMeterProvider, disable: disableMetrics },
  }))
  vi.doMock('@opentelemetry/api-logs', () => ({
    SeverityNumber: { INFO: 9, ERROR: 17 },
    logs: {
      setGlobalLoggerProvider,
      getLogger: vi.fn(() => ({ emit: loggerEmit })),
      disable: disableLogs,
    },
  }))
  vi.doMock('@opentelemetry/sdk-trace-web', () => ({
    WebTracerProvider: vi.fn(function (this: Record<string, unknown>) {
      this.register = register
      this.forceFlush = forceFlush
      this.shutdown = shutdown
    }),
    BatchSpanProcessor: vi.fn(function (exporter: { export: (...args: unknown[]) => void }) {
      batchSpanExporter.current = exporter
    }),
  }))
  vi.doMock('@opentelemetry/exporter-trace-otlp-http', () => ({
    OTLPTraceExporter: otlpTraceExporter,
  }))
  vi.doMock('@opentelemetry/resources', () => ({ resourceFromAttributes }))
  vi.doMock('@opentelemetry/semantic-conventions', () => ({
    ATTR_DEPLOYMENT_ENVIRONMENT_NAME: 'deployment.environment.name',
    ATTR_SERVICE_NAME: 'service.name',
    ATTR_SERVICE_VERSION: 'service.version',
  }))
  vi.doMock('@opentelemetry/instrumentation', () => ({ registerInstrumentations }))
  vi.doMock('@opentelemetry/instrumentation-fetch', () => ({ FetchInstrumentation: vi.fn() }))
  vi.doMock('@opentelemetry/instrumentation-document-load', () => ({
    DocumentLoadInstrumentation: vi.fn(),
  }))
  vi.doMock('@opentelemetry/instrumentation-user-interaction', () => ({
    UserInteractionInstrumentation: vi.fn(),
  }))
  vi.doMock('@opentelemetry/sdk-metrics', () => ({
    MeterProvider: vi.fn(function (this: Record<string, unknown>) {
      this.getMeter = vi.fn(() => meter)
      this.forceFlush = forceFlush
      this.shutdown = shutdown
    }),
    PeriodicExportingMetricReader: vi.fn(),
  }))
  vi.doMock('@opentelemetry/exporter-metrics-otlp-http', () => ({
    OTLPMetricExporter: otlpMetricExporter,
  }))
  vi.doMock('@opentelemetry/sdk-logs', () => ({
    LoggerProvider: vi.fn(function (this: Record<string, unknown>) {
      this.forceFlush = forceFlush
      this.shutdown = shutdown
    }),
    BatchLogRecordProcessor: vi.fn(),
  }))
  vi.doMock('@opentelemetry/exporter-logs-otlp-http', () => ({
    OTLPLogExporter: otlpLogExporter,
  }))
  vi.doMock('web-vitals', () => ({
    onCLS: (callback: Handler) => {
      vitals.cls = callback
    },
    onFCP: (callback: Handler) => {
      vitals.fcp = callback
    },
    onINP: (callback: Handler) => {
      vitals.inp = callback
    },
    onLCP: (callback: Handler) => {
      vitals.lcp = callback
    },
    onTTFB: (callback: Handler) => {
      vitals.ttfb = callback
    },
  }))

  return {
    counters,
    histograms,
    loggerEmit,
    otlpTraceExporter,
    otlpMetricExporter,
    otlpLogExporter,
    traceExporterInstance,
    batchSpanExporter,
    registerInstrumentations,
    resourceFromAttributes,
    setGlobalMeterProvider,
    setGlobalLoggerProvider,
    disableTrace,
    disableMetrics,
    disableLogs,
    register,
    forceFlush,
    shutdown,
    span,
    vitals,
  }
}

async function grantConsent() {
  localStorage.setItem(TELEMETRY_CONSENT_STORAGE_KEY, 'granted')
  vi.stubEnv('VITE_OTEL_EXPORTER_OTLP_ENDPOINT', '/otlp')
  vi.stubEnv('VITE_OTEL_SERVICE_NAME', 'ironrag-web-test')
  vi.stubEnv('VITE_OTEL_SERVICE_VERSION', '1.2.3')
  vi.stubEnv('VITE_OTEL_DEPLOYMENT_ENVIRONMENT', 'test')
}

describe('observability', () => {
  afterEach(() => {
    vi.unstubAllEnvs()
    vi.restoreAllMocks()
    vi.resetModules()
    for (const moduleName of otelModules) {
      vi.doUnmock(moduleName)
    }
    localStorage.clear()
  })

  it('does not load exporters or install handlers without consent', async () => {
    vi.stubEnv('VITE_OTEL_EXPORTER_OTLP_ENDPOINT', '/otlp')
    const sdk = mockSdk()
    const { initObservability } = await import('./observability')

    await initObservability()

    expect(sdk.otlpTraceExporter).not.toHaveBeenCalled()
    expect(sdk.otlpMetricExporter).not.toHaveBeenCalled()
    expect(sdk.otlpLogExporter).not.toHaveBeenCalled()
    expect(sdk.registerInstrumentations).not.toHaveBeenCalled()
    expect(sdk.register).not.toHaveBeenCalled()
  })

  it('does not load exporters after an explicit denial', async () => {
    localStorage.setItem(TELEMETRY_CONSENT_STORAGE_KEY, 'denied')
    vi.stubEnv('VITE_OTEL_EXPORTER_OTLP_ENDPOINT', '/otlp')
    const sdk = mockSdk()
    const { initObservability } = await import('./observability')

    await initObservability()

    expect(sdk.otlpTraceExporter).not.toHaveBeenCalled()
    expect(sdk.otlpLogExporter).not.toHaveBeenCalled()
  })

  it('stays a no-op when no OTLP endpoint was baked into the image', async () => {
    localStorage.setItem(TELEMETRY_CONSENT_STORAGE_KEY, 'granted')
    vi.stubEnv('VITE_OTEL_EXPORTER_OTLP_ENDPOINT', '')
    const sdk = mockSdk()
    const info = vi.spyOn(console, 'info').mockImplementation(() => undefined)
    const { initObservability } = await import('./observability')

    await initObservability()

    expect(sdk.otlpTraceExporter).not.toHaveBeenCalled()
    expect(info).toHaveBeenCalledWith('observability: skipped (no OTEL endpoint)')
  })

  it('creates trace, metric and log exporters on the same-origin OTLP path', async () => {
    await grantConsent()
    const sdk = mockSdk()
    const info = vi.spyOn(console, 'info').mockImplementation(() => undefined)
    const { initObservability } = await import('./observability')

    await initObservability()
    window.dispatchEvent(new Event('load'))

    expect(sdk.otlpTraceExporter).toHaveBeenCalledWith({
      url: expect.stringMatching(/\/otlp\/v1\/traces$/),
    })
    expect(sdk.otlpMetricExporter).toHaveBeenCalledWith({
      url: expect.stringMatching(/\/otlp\/v1\/metrics$/),
    })
    expect(sdk.otlpLogExporter).toHaveBeenCalledWith({
      url: expect.stringMatching(/\/otlp\/v1\/logs$/),
    })
    expect(sdk.setGlobalMeterProvider).toHaveBeenCalled()
    expect(sdk.setGlobalLoggerProvider).toHaveBeenCalled()
    expect(sdk.register).toHaveBeenCalled()
    expect(sdk.registerInstrumentations).toHaveBeenCalled()
    expect(sdk.counters.has('ironrag.web.js_errors')).toBe(true)
    expect(sdk.counters.has('ironrag.web.page_loads')).toBe(true)
    expect(sdk.counters.get('ironrag.web.page_loads')?.add).toHaveBeenCalledWith(
      1,
      expect.objectContaining({ 'page.outcome': 'ok' }),
    )
    expect(sdk.loggerEmit).toHaveBeenCalledWith(expect.objectContaining({ body: 'session_start' }))
    expect(info).toHaveBeenCalledWith('observability: enabled (test)')

    const secondInit = initObservability()
    await secondInit
    expect(sdk.otlpTraceExporter).toHaveBeenCalledTimes(1)
  })

  it('uses the app version when the OTel service version build arg is empty', async () => {
    await grantConsent()
    vi.stubEnv('VITE_OTEL_SERVICE_VERSION', '')
    vi.stubEnv('VITE_APP_VERSION', '0.6.2-rc.1')
    const sdk = mockSdk()
    vi.spyOn(console, 'info').mockImplementation(() => undefined)
    const { initObservability } = await import('./observability')

    await initObservability()

    expect(sdk.resourceFromAttributes).toHaveBeenCalledWith(
      expect.objectContaining({ 'service.version': '0.6.2-rc.1' }),
    )
  })

  it('records web vitals with rating and unit-appropriate instruments', async () => {
    await grantConsent()
    const sdk = mockSdk()
    vi.spyOn(console, 'info').mockImplementation(() => undefined)
    const { initObservability } = await import('./observability')
    await initObservability()

    sdk.vitals.lcp?.({ value: 2500, rating: 'good' })
    sdk.vitals.cls?.({ value: 0.08, rating: 'needs-improvement' })

    expect(sdk.histograms.get('ironrag.web.vitals.duration')?.record).toHaveBeenCalledWith(
      2500,
      expect.objectContaining({ 'vital.name': 'lcp', 'vital.rating': 'good' }),
    )
    expect(sdk.histograms.get('ironrag.web.vitals.score')?.record).toHaveBeenCalledWith(
      0.08,
      expect.objectContaining({ 'vital.name': 'cls' }),
    )
  })

  it('sanitises product events before they reach the log exporter', async () => {
    await grantConsent()
    const sdk = mockSdk()
    vi.spyOn(console, 'info').mockImplementation(() => undefined)
    const { initObservability } = await import('./observability')
    await initObservability()
    const { trackEvent } = await import('./telemetry/events')

    trackEvent('page_view', {
      feature: 'navigation',
      email: 'alice@example.com',
      route: 'https://graph.piping.space/assistant?token=secret#frag',
    })

    const record = sdk.loggerEmit.mock.calls.at(-1)?.[0] as {
      body: string
      attributes: Record<string, string>
    }
    expect(record.body).toBe('page_view')
    expect(record.attributes).toMatchObject({
      'event.name': 'page_view',
      'page.route': '/',
      feature: 'navigation',
    })
    expect(JSON.stringify(record.attributes)).not.toContain('alice@example.com')
    expect(JSON.stringify(record.attributes)).not.toContain('secret')
  })

  it('turns uncaught errors into a counter and a redacted error log', async () => {
    await grantConsent()
    const sdk = mockSdk()
    vi.spyOn(console, 'info').mockImplementation(() => undefined)
    const { initObservability } = await import('./observability')
    await initObservability()

    window.dispatchEvent(new ErrorEvent('error', { error: new TypeError('boom at a@b.co') }))

    expect(sdk.counters.get('ironrag.web.js_errors')?.add).toHaveBeenCalledWith(
      1,
      expect.objectContaining({ 'error.source': 'window.error', 'error.type': 'TypeError' }),
    )
    expect(sdk.loggerEmit).toHaveBeenCalledWith(
      expect.objectContaining({
        severityNumber: 17,
        body: 'boom at [redacted-email]',
      }),
    )
  })

  it('captures React tree exceptions as spans and error logs', async () => {
    await grantConsent()
    const sdk = mockSdk()
    vi.spyOn(console, 'info').mockImplementation(() => undefined)
    const { captureUiException, initObservability } = await import('./observability')
    await initObservability()

    captureUiException(new Error('render failed'), { feature: 'react-tree' })

    expect(sdk.span.recordException).toHaveBeenCalled()
    expect(sdk.span.setStatus).toHaveBeenCalledWith({ code: 2 })
    expect(sdk.span.setAttributes).toHaveBeenCalledWith({ feature: 'react-tree' })
    expect(sdk.span.end).toHaveBeenCalled()
    expect(sdk.loggerEmit).toHaveBeenCalledWith(
      expect.objectContaining({ severityNumber: 17, body: 'render failed' }),
    )
  })

  it('stops accepting events and shuts exporters down when consent is withdrawn', async () => {
    await grantConsent()
    const sdk = mockSdk()
    vi.spyOn(console, 'info').mockImplementation(() => undefined)
    const { disableObservability, initObservability } = await import('./observability')
    await initObservability()
    const { trackEvent } = await import('./telemetry/events')

    await disableObservability()
    sdk.loggerEmit.mockClear()
    trackEvent('page_view', { feature: 'navigation' })

    expect(sdk.loggerEmit).not.toHaveBeenCalled()
    expect(sdk.shutdown).toHaveBeenCalled()
    expect(sdk.disableTrace).toHaveBeenCalled()
    expect(sdk.disableMetrics).toHaveBeenCalled()
    expect(sdk.disableLogs).toHaveBeenCalled()
  })

  it('does not start while an account preference is loading or failed', async () => {
    vi.stubEnv('VITE_OTEL_EXPORTER_OTLP_ENDPOINT', '/otlp')
    const sdk = mockSdk()
    const consent = await import('./telemetry/consent')
    consent.beginAccountTelemetryConsent('principal-a')
    const { initObservability } = await import('./observability')

    await initObservability()
    expect(sdk.otlpTraceExporter).not.toHaveBeenCalled()

    consent.failAccountTelemetryConsent('principal-a')
    await initObservability()
    expect(sdk.otlpTraceExporter).not.toHaveBeenCalled()
    expect(sdk.registerInstrumentations).not.toHaveBeenCalled()
  })

  it('drops a queued span batch when consent is withdrawn', async () => {
    await grantConsent()
    const sdk = mockSdk()
    vi.spyOn(console, 'info').mockImplementation(() => undefined)
    const { disableObservability, initObservability } = await import('./observability')
    await initObservability()
    const exporter = sdk.batchSpanExporter.current
    expect(exporter).not.toBeNull()

    exporter?.export([], vi.fn())
    expect(sdk.traceExporterInstance.export).toHaveBeenCalledTimes(1)

    await disableObservability()
    exporter?.export([], vi.fn())
    expect(sdk.traceExporterInstance.export).toHaveBeenCalledTimes(1)
  })

  it('serialises a re-enable behind provider shutdown instead of registering into a dying global', async () => {
    await grantConsent()
    const sdk = mockSdk()
    vi.spyOn(console, 'info').mockImplementation(() => undefined)
    const { disableObservability, initObservability } = await import('./observability')
    await initObservability()

    let releaseShutdown!: () => void
    sdk.shutdown.mockImplementationOnce(
      () =>
        new Promise<void>((resolve) => {
          releaseShutdown = resolve
        }),
    )

    const disabling = disableObservability()
    const reEnabling = initObservability()
    await vi.waitFor(() => expect(releaseShutdown).toBeTypeOf('function'))
    releaseShutdown()
    await Promise.all([disabling, reEnabling])

    expect(sdk.otlpTraceExporter).toHaveBeenCalledTimes(2)
    expect(sdk.disableMetrics.mock.invocationCallOrder[0]).toBeLessThan(
      sdk.setGlobalMeterProvider.mock.invocationCallOrder[1]!,
    )
  })

  it('can start a fresh provider after withdrawal instead of reusing a shut-down one', async () => {
    await grantConsent()
    const sdk = mockSdk()
    vi.spyOn(console, 'info').mockImplementation(() => undefined)
    const { disableObservability, initObservability } = await import('./observability')
    await initObservability()
    await disableObservability()

    await initObservability()
    window.dispatchEvent(new Event('load'))

    expect(sdk.otlpTraceExporter).toHaveBeenCalledTimes(2)
    expect(sdk.register).toHaveBeenCalledTimes(2)
    expect(sdk.setGlobalMeterProvider).toHaveBeenCalledTimes(2)
    expect(sdk.setGlobalLoggerProvider).toHaveBeenCalledTimes(2)
    expect(sdk.loggerEmit).toHaveBeenCalledWith(expect.objectContaining({ body: 'session_start' }))
  })
})
