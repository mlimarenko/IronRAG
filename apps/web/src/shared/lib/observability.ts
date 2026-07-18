type ObservabilityHint = {
  feature?: string
  sessionId?: string
  libraryId?: string
}

type SpanLike = {
  recordException(err: unknown): void
  setAttributes(attrs: Record<string, string>): void
  setStatus(status: { code: number }): void
  end(): void
}

type TracerLike = {
  startSpan(name: string): SpanLike
}

let tracer: TracerLike | null = null
let spanStatusError: number | null = null
let initPromise: Promise<void> | null = null

const readEnv = (name: string): string | undefined => {
  const value = import.meta.env?.[name]
  return typeof value === 'string' ? value.trim() : undefined
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

async function createZoneContextManager() {
  if (!hasZone()) {
    return undefined
  }
  const { ZoneContextManager } = await import('@opentelemetry/context-zone-peer-dep')
  return new ZoneContextManager()
}

async function startObservability(): Promise<void> {
  const endpoint = readEnv('VITE_OTEL_EXPORTER_OTLP_ENDPOINT')
  if (!endpoint) {
    console.info('observability: skipped (no OTEL endpoint)')
    return
  }

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
  ])

  const resource = resources.resourceFromAttributes({
    [semanticConventions.ATTR_SERVICE_NAME]: serviceName,
    [semanticConventions.ATTR_SERVICE_VERSION]: serviceVersion,
    [semanticConventions.ATTR_DEPLOYMENT_ENVIRONMENT_NAME]: deploymentEnvironment,
  })

  const provider = new traceWeb.WebTracerProvider({
    resource,
    spanProcessors: [
      new traceWeb.BatchSpanProcessor(new exporter.OTLPTraceExporter({ url: endpoint })),
    ],
  })

  const contextManager = await createZoneContextManager()
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
  console.info(`observability: enabled (${deploymentEnvironment})`)
}

export async function initObservability(): Promise<void> {
  initPromise ??= startObservability()
  await initPromise
}

export function captureUiException(err: unknown, hint?: ObservabilityHint): void {
  if (!tracer || spanStatusError === null) {
    console.error('[ui]', err, hint ?? {})
    return
  }

  const span = tracer.startSpan('ui.exception')
  span.recordException(err)
  span.setStatus({ code: spanStatusError })
  const attrs = buildHintAttributes(hint)
  if (Object.keys(attrs).length > 0) {
    span.setAttributes(attrs)
  }
  span.end()
}
