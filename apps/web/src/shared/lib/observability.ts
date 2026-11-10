import type { Attributes, Counter, Meter, SpanStatusCode, Tracer } from '@opentelemetry/api'
import type { Logger } from '@opentelemetry/api-logs'

import { createProductSignalsSpanProcessor } from './telemetry/apiSignals'
import {
  isTelemetryAllowed,
  subscribeTelemetryConsent,
  type TelemetryConsent,
} from './telemetry/consent'
import { describeError, installGlobalErrorHandlers } from './telemetry/errors'
import { setProductEventSink, trackEvent } from './telemetry/events'
import { normalizeRoute, redactText, sanitizeAttributes } from './telemetry/redaction'
import { telemetrySessionId } from './telemetry/session'
import { observeWebVitals } from './telemetry/webVitals'

type ObservabilityHint = {
  feature?: string
  sessionId?: string
  libraryId?: string
}

type ShutdownHook = {
  shutdown(): Promise<void>
}

const readEnv = (name: string): string | undefined => {
  const value = import.meta.env?.[name]
  if (typeof value !== 'string') return undefined
  const trimmed = value.trim()
  // An empty build arg is absent, not an empty override: fall back to the next
  // source instead of baking an empty service.version into the bundle.
  return trimmed.length === 0 ? undefined : trimmed
}

const escapeRegex = (value: string) => value.replace(/[|\\{}()[\]^$+*?.]/g, String.raw`\$&`)

const buildHintAttributes = (hint?: ObservabilityHint) => {
  const attrs: Record<string, string> = {}
  if (hint?.feature) {
    attrs.feature = hint.feature
  }
  if (hint?.sessionId) {
    attrs['session.id'] = hint.sessionId
  }
  if (hint?.libraryId) {
    attrs['library.id'] = hint.libraryId
  }
  return attrs
}

const hasZone = () => (globalThis as typeof globalThis & { Zone?: unknown }).Zone !== undefined

type ExporterLike = {
  export: (...args: unknown[]) => void
  forceFlush?: () => Promise<void>
  shutdown: () => Promise<void>
}

/**
 * Drops queued exports after consent is withdrawn. The OTel SDK flushes its
 * batch processors during shutdown; without this gate a revocation would send
 * the events that were pending at that moment.
 */
function consentGatedExporter<T extends object>(exporter: T): T {
  return new Proxy(exporter, {
    get(target, property, receiver) {
      if (property === 'export') {
        return (...args: unknown[]) => {
          if (!exportEnabled || !isTelemetryAllowed()) {
            const callback = args[1]
            if (typeof callback === 'function') {
              ;(callback as (result: { code: number }) => void)({ code: 0 })
            }
            return
          }
          return (target as ExporterLike).export(...args)
        }
      }
      const value = Reflect.get(target, property, receiver)
      return typeof value === 'function' ? value.bind(target) : value
    },
  })
}

async function createZoneContextManager() {
  if (!hasZone()) {
    return undefined
  }
  const { ZoneContextManager } = await import('@opentelemetry/context-zone-peer-dep')
  return new ZoneContextManager()
}

/** Accept an OTLP base URL or a signal-specific URL and return the base. */
const otlpBaseUrl = (value: string): string => {
  const withoutTrailing = value.replace(/\/+$/, '')
  return withoutTrailing.replace(/\/v1\/(?:traces|metrics|logs)$/i, '')
}

const otlpSignalUrl = (value: string, signal: 'traces' | 'metrics' | 'logs'): string => {
  const base = otlpBaseUrl(value)
  if (/^https?:\/\//i.test(base)) return `${base}/v1/${signal}`
  const origin = typeof window === 'undefined' ? 'http://localhost' : window.location.origin
  return new URL(`${base}/v1/${signal}`, origin).toString()
}

let tracer: Tracer | null = null
let logger: Logger | null = null
let spanStatusError: SpanStatusCode | null = null
let errorSeverity = 17
let initPromise: Promise<void> | null = null
let started = false
let exportEnabled = false
let consentEpoch = 0
let disableGlobalProviders: (() => void) | null = null
let shutdownHooks: ShutdownHook[] = []
let removeErrorHandlers: (() => void) | null = null
let removeFlushListeners: (() => void) | null = null
let consentWatcher: (() => void) | null = null
let pageErrored = false
let jsErrorCounter: Counter | null = null
let pageLoadCounter: Counter | null = null

/**
 * Serialises provider start and shutdown. OTel globals are process-wide, so a
 * second start that overlaps a shutdown can register into a global the first
 * transition is about to remove.
 */
let transition: Promise<void> = Promise.resolve()

function runExclusive(task: () => Promise<void>): Promise<void> {
  const next = transition.then(task, task)
  transition = next.catch(() => undefined)
  return next
}

async function startObservability(epoch: number): Promise<void> {
  const endpoint = readEnv('VITE_OTEL_EXPORTER_OTLP_ENDPOINT')
  if (!endpoint) {
    console.info('observability: skipped (no OTEL endpoint)')
    return
  }
  if (consentEpoch !== epoch || !isTelemetryAllowed()) return

  const serviceName = readEnv('VITE_OTEL_SERVICE_NAME') ?? 'ironrag-web'
  const serviceVersion =
    readEnv('VITE_OTEL_SERVICE_VERSION') ?? readEnv('VITE_APP_VERSION') ?? '0.0.0'
  const deploymentEnvironment =
    readEnv('VITE_OTEL_DEPLOYMENT_ENVIRONMENT') ?? import.meta.env?.MODE ?? 'development'

  const [
    api,
    traceWeb,
    exporter,
    resources,
    semanticConventions,
    instrumentation,
    fetchInstrumentation,
    documentLoadInstrumentation,
    userInteractionInstrumentation,
    sdkMetrics,
    metricsExporter,
    sdkLogs,
    logsExporter,
    apiLogs,
  ] = await Promise.all([
    import('@opentelemetry/api'),
    import('@opentelemetry/sdk-trace-web'),
    import('@opentelemetry/exporter-trace-otlp-http'),
    import('@opentelemetry/resources'),
    import('@opentelemetry/semantic-conventions'),
    import('@opentelemetry/instrumentation'),
    import('@opentelemetry/instrumentation-fetch'),
    import('@opentelemetry/instrumentation-document-load'),
    import('@opentelemetry/instrumentation-user-interaction'),
    import('@opentelemetry/sdk-metrics'),
    import('@opentelemetry/exporter-metrics-otlp-http'),
    import('@opentelemetry/sdk-logs'),
    import('@opentelemetry/exporter-logs-otlp-http'),
    import('@opentelemetry/api-logs'),
  ])

  const resource = resources.resourceFromAttributes({
    [semanticConventions.ATTR_SERVICE_NAME]: serviceName,
    [semanticConventions.ATTR_SERVICE_VERSION]: serviceVersion,
    [semanticConventions.ATTR_DEPLOYMENT_ENVIRONMENT_NAME]: deploymentEnvironment,
  })

  const clearGlobalProviders = () => {
    api.trace.disable()
    api.metrics.disable()
    apiLogs.logs.disable()
  }
  disableGlobalProviders = clearGlobalProviders

  const loggerProvider = new sdkLogs.LoggerProvider({
    resource,
    processors: [
      new sdkLogs.BatchLogRecordProcessor({
        exporter: consentGatedExporter(
          new logsExporter.OTLPLogExporter({ url: otlpSignalUrl(endpoint, 'logs') }),
        ),
      }),
    ],
  })
  apiLogs.logs.setGlobalLoggerProvider(loggerProvider)

  const meterProvider = new sdkMetrics.MeterProvider({
    resource,
    readers: [
      new sdkMetrics.PeriodicExportingMetricReader({
        exporter: consentGatedExporter(
          new metricsExporter.OTLPMetricExporter({
            url: otlpSignalUrl(endpoint, 'metrics'),
          }),
        ),
        exportIntervalMillis: 60_000,
      }),
    ],
  })
  api.metrics.setGlobalMeterProvider(meterProvider)

  const productSignals = createProductSignalsSpanProcessor({
    meter: meterProvider.getMeter(serviceName, serviceVersion),
  })

  const provider = new traceWeb.WebTracerProvider({
    resource,
    spanProcessors: [
      new traceWeb.BatchSpanProcessor(
        consentGatedExporter(
          new exporter.OTLPTraceExporter({ url: otlpSignalUrl(endpoint, 'traces') }),
        ),
      ),
      productSignals,
    ],
  })

  if (consentEpoch !== epoch || !isTelemetryAllowed()) {
    clearGlobalProviders()
    await Promise.allSettled([
      provider.shutdown(),
      meterProvider.shutdown(),
      loggerProvider.shutdown(),
    ])
    return
  }

  const contextManager = await createZoneContextManager()
  if (consentEpoch !== epoch || !isTelemetryAllowed()) {
    clearGlobalProviders()
    await Promise.allSettled([
      provider.shutdown(),
      meterProvider.shutdown(),
      loggerProvider.shutdown(),
    ])
    return
  }
  if (contextManager) {
    provider.register({ contextManager })
  } else {
    provider.register()
  }

  instrumentation.registerInstrumentations({
    instrumentations: [
      new fetchInstrumentation.FetchInstrumentation({
        ignoreUrls: [new RegExp(`^${escapeRegex(endpoint)}`)],
      }),
      new documentLoadInstrumentation.DocumentLoadInstrumentation(),
      new userInteractionInstrumentation.UserInteractionInstrumentation(),
    ],
    tracerProvider: provider,
  })

  tracer = api.trace.getTracer(serviceName, serviceVersion)
  spanStatusError = api.SpanStatusCode.ERROR
  errorSeverity = apiLogs.SeverityNumber.ERROR
  logger = apiLogs.logs.getLogger(serviceName, serviceVersion)

  const meter: Meter = meterProvider.getMeter(serviceName, serviceVersion)
  jsErrorCounter = meter.createCounter('ironrag.web.js_errors', {
    description: 'Uncaught JavaScript exceptions and unhandled promise rejections.',
  })
  pageLoadCounter = meter.createCounter('ironrag.web.page_loads', {
    description: 'Completed page loads by outcome, used for release health.',
  })
  const vitalDuration = meter.createHistogram('ironrag.web.vitals.duration', {
    unit: 'ms',
    description: 'Core Web Vitals measured in milliseconds (LCP, INP, FCP, TTFB).',
  })
  const vitalScore = meter.createHistogram('ironrag.web.vitals.score', {
    description: 'Cumulative layout shift score.',
  })

  setProductEventSink((name, attributes) => {
    logger?.emit({
      severityNumber: apiLogs.SeverityNumber.INFO,
      severityText: 'INFO',
      body: name,
      attributes,
    })
  })

  removeErrorHandlers = installGlobalErrorHandlers((details) => {
    pageErrored = true
    const route = normalizeRoute(window.location.pathname)
    jsErrorCounter?.add(1, {
      'error.source': details.source,
      'error.type': details.type,
      'page.route': route,
    })
    const attributes = sanitizeAttributes({
      'event.name': 'js_error',
      'error.source': details.source,
      'error.type': details.type,
      'page.route': route,
      'session.id': telemetrySessionId(),
    })
    // Stacks are kept long enough for symbolication but still redacted.
    attributes['error.stack'] = redactText(details.stack, 2048)
    attributes['error.message'] = redactText(details.message, 512)
    logger?.emit({
      severityNumber: apiLogs.SeverityNumber.ERROR,
      severityText: 'ERROR',
      body: details.message,
      attributes,
    })
  })

  observeWebVitals(({ name, value, rating }) => {
    const attributes: Attributes = {
      'vital.name': name,
      'vital.rating': rating,
      'page.route': normalizeRoute(window.location.pathname),
    }
    if (name === 'cls') {
      vitalScore.record(value, attributes)
    } else {
      vitalDuration.record(value, attributes)
    }
  })

  const reportPageLoad = () => {
    pageLoadCounter?.add(1, {
      'page.outcome': pageErrored ? 'error' : 'ok',
      'page.route': normalizeRoute(window.location.pathname),
    })
  }
  if (document.readyState === 'complete') {
    reportPageLoad()
  } else {
    window.addEventListener('load', reportPageLoad, { once: true })
  }

  const flushAll = () => {
    void Promise.allSettled([
      provider.forceFlush(),
      meterProvider.forceFlush(),
      loggerProvider.forceFlush(),
    ])
  }
  const onVisibilityChange = () => {
    if (document.visibilityState === 'hidden') flushAll()
  }
  window.addEventListener('pagehide', flushAll)
  document.addEventListener('visibilitychange', onVisibilityChange)
  removeFlushListeners = () => {
    window.removeEventListener('pagehide', flushAll)
    document.removeEventListener('visibilitychange', onVisibilityChange)
  }

  if (consentEpoch !== epoch || !isTelemetryAllowed()) {
    removeErrorHandlers?.()
    removeErrorHandlers = null
    removeFlushListeners?.()
    removeFlushListeners = null
    setProductEventSink(null)
    clearGlobalProviders()
    await Promise.allSettled([
      provider.shutdown(),
      meterProvider.shutdown(),
      loggerProvider.shutdown(),
    ])
    return
  }

  shutdownHooks = [provider, meterProvider, loggerProvider]
  exportEnabled = true
  started = true
  trackEvent('session_start', { feature: 'frontend' })
  console.info(`observability: enabled (${deploymentEnvironment})`)
}

export function initObservability(): Promise<void> {
  return runExclusive(async () => {
    if (started) return
    if (!isTelemetryAllowed()) return
    const epoch = consentEpoch
    initPromise = startObservability(epoch).catch((error: unknown) => {
      console.warn('observability: failed to start', error)
    })
    await initPromise
    if (!started) initPromise = null
  })
}

/**
 * React to consent changes made in this tab or another one. Call once from the
 * application entry point, after the first init attempt.
 */
export function watchTelemetryConsent(): void {
  consentWatcher ??= subscribeTelemetryConsent((consent: TelemetryConsent) => {
    if (consent === 'granted') {
      void initObservability()
    } else {
      void disableObservability()
    }
  })
}

export function disableObservability(): Promise<void> {
  // Invalidate an in-flight start synchronously; its next epoch check aborts.
  consentEpoch += 1
  exportEnabled = false
  return runExclusive(async () => {
    const wasActive = started || initPromise !== null || shutdownHooks.length > 0
    removeErrorHandlers?.()
    removeErrorHandlers = null
    removeFlushListeners?.()
    removeFlushListeners = null
    setProductEventSink(null)
    tracer = null
    logger = null
    spanStatusError = null
    jsErrorCounter = null
    pageLoadCounter = null
    started = false
    const hooks = shutdownHooks
    shutdownHooks = []
    await Promise.allSettled(hooks.map((hook) => hook.shutdown()))
    disableGlobalProviders?.()
    disableGlobalProviders = null
    initPromise = null
    if (wasActive) {
      console.info('observability: disabled (consent withdrawn)')
    }
  })
}

export function captureUiException(err: unknown, hint?: ObservabilityHint): void {
  if (!tracer || spanStatusError === null) {
    console.error('[ui]', err, hint ?? {})
    return
  }

  const span = tracer.startSpan('ui.exception')
  span.recordException(err instanceof Error || typeof err === 'string' ? err : String(err))
  span.setStatus({ code: spanStatusError })
  const attrs = buildHintAttributes(hint)
  if (Object.keys(attrs).length > 0) {
    span.setAttributes(attrs)
  }
  span.end()

  const described = describeError(err)
  jsErrorCounter?.add(1, {
    'error.source': 'react',
    'error.type': described.type,
    'page.route': normalizeRoute(window.location.pathname),
  })
  const attributes = sanitizeAttributes({
    'event.name': 'js_error',
    'error.source': 'react',
    'error.type': described.type,
    'page.route': normalizeRoute(window.location.pathname),
    'session.id': telemetrySessionId(),
    feature: hint?.feature,
  })
  attributes['error.stack'] = redactText(described.stack, 2048)
  attributes['error.message'] = redactText(described.message, 512)
  logger?.emit({
    severityNumber: errorSeverity,
    severityText: 'ERROR',
    body: described.message,
    attributes,
  })
}
