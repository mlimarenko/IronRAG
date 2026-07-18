import { afterEach, describe, expect, it, vi } from 'vitest'

const otelModules = [
  '@opentelemetry/api',
  '@opentelemetry/sdk-trace-web',
  '@opentelemetry/exporter-trace-otlp-http',
  '@opentelemetry/resources',
  '@opentelemetry/semantic-conventions',
  '@opentelemetry/semantic-conventions/incubating',
  '@opentelemetry/instrumentation',
  '@opentelemetry/instrumentation-fetch',
  '@opentelemetry/instrumentation-document-load',
  '@opentelemetry/instrumentation-user-interaction',
  '@opentelemetry/context-zone-peer-dep',
]

describe('observability', () => {
  afterEach(() => {
    vi.unstubAllEnvs()
    vi.restoreAllMocks()
    vi.resetModules()
    for (const moduleName of otelModules) {
      vi.doUnmock(moduleName)
    }
  })

  it('is a no-op when VITE_OTEL_EXPORTER_OTLP_ENDPOINT is empty', async () => {
    vi.stubEnv('VITE_OTEL_EXPORTER_OTLP_ENDPOINT', '')
    const info = vi.spyOn(console, 'info').mockImplementation(() => undefined)
    const { initObservability } = await import('./observability')

    await expect(initObservability()).resolves.toBeUndefined()
    expect(info).toHaveBeenCalledWith('observability: skipped (no OTEL endpoint)')
  })

  it('falls back to console.error when no OTEL endpoint is configured', async () => {
    vi.stubEnv('VITE_OTEL_EXPORTER_OTLP_ENDPOINT', '')
    const { captureUiException, initObservability } = await import('./observability')

    await initObservability()
    const err = vi.spyOn(console, 'error').mockImplementation(() => undefined)
    captureUiException(new Error('boom'), { feature: 'test' })
    expect(err).toHaveBeenCalled()
  })

  it('initializes the OTEL exporter when an endpoint is configured', async () => {
    vi.stubEnv('VITE_OTEL_EXPORTER_OTLP_ENDPOINT', 'https://otel.example/v1/traces')
    vi.stubEnv('VITE_OTEL_SERVICE_NAME', 'ironrag-web-test')
    vi.stubEnv('VITE_OTEL_SERVICE_VERSION', '1.2.3')
    vi.stubEnv('VITE_OTEL_DEPLOYMENT_ENVIRONMENT', 'test')

    const span = {
      recordException: vi.fn(),
      setAttributes: vi.fn(),
      setStatus: vi.fn(),
      end: vi.fn(),
    }
    const tracer = { startSpan: vi.fn(() => span) }
    const register = vi.fn()
    const getTracer = vi.fn(() => tracer)
    const registerInstrumentations = vi.fn()
    const resourceFromAttributes = vi.fn((attrs) => ({ attrs }))
    const otlpTraceExporter = vi.fn()
    const batchSpanProcessor = vi.fn()
    const fetchInstrumentation = vi.fn()
    const documentLoadInstrumentation = vi.fn()
    const userInteractionInstrumentation = vi.fn()
    const info = vi.spyOn(console, 'info').mockImplementation(() => undefined)

    vi.doMock('@opentelemetry/api', () => ({
      SpanStatusCode: { ERROR: 2 },
      trace: { getTracer },
    }))
    vi.doMock('@opentelemetry/sdk-trace-web', () => ({
      BatchSpanProcessor: batchSpanProcessor,
      WebTracerProvider: vi.fn(function WebTracerProvider(this: { register: typeof register }) {
        this.register = register
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
    vi.doMock('@opentelemetry/instrumentation', () => ({
      registerInstrumentations,
    }))
    vi.doMock('@opentelemetry/instrumentation-fetch', () => ({
      FetchInstrumentation: fetchInstrumentation,
    }))
    vi.doMock('@opentelemetry/instrumentation-document-load', () => ({
      DocumentLoadInstrumentation: documentLoadInstrumentation,
    }))
    vi.doMock('@opentelemetry/instrumentation-user-interaction', () => ({
      UserInteractionInstrumentation: userInteractionInstrumentation,
    }))

    const { captureUiException, initObservability } = await import('./observability')

    await expect(initObservability()).resolves.toBeUndefined()
    expect(otlpTraceExporter).toHaveBeenCalledWith({
      url: 'https://otel.example/v1/traces',
    })
    expect(registerInstrumentations).toHaveBeenCalled()
    expect(info).toHaveBeenCalledWith('observability: enabled (test)')

    captureUiException(new Error('boom'), {
      feature: 'test',
      libraryId: 'library-1',
      sessionId: 'session-1',
    })

    expect(getTracer).toHaveBeenCalledWith('ironrag-web-test', '1.2.3')
    expect(tracer.startSpan).toHaveBeenCalledWith('ui.exception')
    expect(span.recordException).toHaveBeenCalled()
    expect(span.setStatus).toHaveBeenCalledWith({ code: 2 })
    expect(span.setAttributes).toHaveBeenCalledWith({
      feature: 'test',
      'library.id': 'library-1',
      'session.id': 'session-1',
    })
    expect(span.end).toHaveBeenCalled()
  })
})
