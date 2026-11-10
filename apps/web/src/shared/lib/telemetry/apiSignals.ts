import type { Attributes } from '@opentelemetry/api'
import type { ReadableSpan, SpanProcessor } from '@opentelemetry/sdk-trace-base'

import { trackEvent } from './events'
import { normalizeRoute } from './redaction'

type CounterLike = {
  add(value: number, attributes?: Attributes): void
}

type MeterLike = {
  createCounter(name: string, options?: { description?: string }): CounterLike
}

type ProductRoute = {
  method: string
  pattern: RegExp
  route: string
  successEvent: string
  failureEvent: string
}

// SpanKind.CLIENT. Importing the numeric constant would pull the tracing SDK
// into this module at runtime; the value is stable in the OTel specification.
const CLIENT_SPAN_KIND = 2

const PRODUCT_ROUTES: ProductRoute[] = [
  {
    method: 'POST',
    pattern: /^\/v1\/iam\/session$/,
    route: '/v1/iam/session',
    successEvent: 'login_succeeded',
    failureEvent: 'login_failed',
  },
  {
    method: 'POST',
    pattern: /^\/v1\/query\/libraries\/[^/]+\/sessions$/,
    route: '/v1/query/libraries/:libraryId/sessions',
    successEvent: 'query_session_started',
    failureEvent: 'query_session_failed',
  },
  {
    method: 'POST',
    pattern: /^\/v1\/query\/sessions\/[^/]+\/turns$/,
    route: '/v1/query/sessions/:sessionId/turns',
    successEvent: 'query_completed',
    failureEvent: 'query_failed',
  },
  {
    method: 'POST',
    pattern: /^\/v1\/content\/libraries\/[^/]+\/documents$/,
    route: '/v1/content/libraries/:libraryId/documents',
    successEvent: 'document_uploaded',
    failureEvent: 'document_upload_failed',
  },
  {
    method: 'POST',
    pattern: /^\/v1\/catalog\/workspaces\/[^/]+\/libraries$/,
    route: '/v1/catalog/workspaces/:workspaceId/libraries',
    successEvent: 'library_created',
    failureEvent: 'library_create_failed',
  },
  {
    method: 'POST',
    pattern: /^\/v1\/ingest\/libraries\/[^/]+\/jobs$/,
    route: '/v1/ingest/libraries/:libraryId/jobs',
    successEvent: 'ingest_started',
    failureEvent: 'ingest_start_failed',
  },
]

const requestPath = (span: ReadableSpan): string => {
  const url = span.attributes['url.full']
  if (typeof url !== 'string' || url.length === 0) return ''
  try {
    const origin = typeof window === 'undefined' ? 'http://localhost' : window.location.origin
    return new URL(url, origin).pathname
  } catch {
    return url.split('?')[0] ?? ''
  }
}

const statusCode = (span: ReadableSpan): number | null => {
  const raw = span.attributes['http.response.status_code']
  if (typeof raw === 'number') return raw
  if (typeof raw === 'string') {
    const parsed = Number(raw)
    return Number.isFinite(parsed) ? parsed : null
  }
  return null
}

const durationMs = (span: ReadableSpan): number => {
  const [seconds, nanos] = span.duration
  return Math.round(seconds * 1000 + nanos / 1_000_000)
}

/**
 * Derives client-observed API failures and product events from the HTTP spans
 * created by the fetch instrumentation, so feature code does not have to call
 * analytics by hand for every request.
 */
export function createProductSignalsSpanProcessor({ meter }: { meter: MeterLike }): SpanProcessor {
  const apiFailures = meter.createCounter('ironrag.web.api.failures', {
    description: 'Browser-observed API responses with a 4xx/5xx status or a network error.',
  })

  return {
    onStart() {},
    onEnd(span: ReadableSpan) {
      if (Number(span.kind) !== CLIENT_SPAN_KIND) return
      const method = span.attributes['http.request.method']
      if (typeof method !== 'string') return
      const path = requestPath(span)
      if (!path.startsWith('/v1/')) return

      const status = statusCode(span)
      const failed = status === null || status >= 400
      if (failed) {
        const errorType = status === null ? 'network' : status >= 500 ? 'http_5xx' : 'http_4xx'
        apiFailures.add(1, {
          'http.request.method': method,
          'http.route': normalizeRoute(path),
          'http.response.status_code': status ?? 'none',
          'error.type': errorType,
        })
      }

      const productRoute = PRODUCT_ROUTES.find(
        (candidate) => candidate.method === method && candidate.pattern.test(path),
      )
      if (!productRoute) return

      trackEvent(failed ? productRoute.failureEvent : productRoute.successEvent, {
        feature: 'api',
        'http.route': productRoute.route,
        'http.response.status_code': status ?? 'none',
        'error.type': failed ? (status === null ? 'network' : 'http_error') : 'none',
        duration_ms: durationMs(span),
      })
    },
    shutdown() {
      return Promise.resolve()
    },
    forceFlush() {
      return Promise.resolve()
    },
  }
}
