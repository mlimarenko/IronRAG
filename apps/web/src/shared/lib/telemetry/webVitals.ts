import { onCLS, onFCP, onINP, onLCP, onTTFB } from 'web-vitals'

type WebVitalName = 'cls' | 'fcp' | 'inp' | 'lcp' | 'ttfb'
type WebVitalRating = 'good' | 'needs-improvement' | 'poor'

export type WebVitalSample = {
  name: WebVitalName
  value: number
  rating: WebVitalRating
}

/**
 * Subscribe to Core Web Vitals. `web-vitals` reports each metric once when it
 * is final (CLS may report again after returning to a backgrounded page), so
 * every call is a complete sample rather than a delta.
 */
export function observeWebVitals(record: (sample: WebVitalSample) => void): void {
  const report = (name: WebVitalName) => (metric: { value: number; rating: WebVitalRating }) => {
    record({ name, value: metric.value, rating: metric.rating })
  }
  onCLS(report('cls'))
  onFCP(report('fcp'))
  onINP(report('inp'))
  onLCP(report('lcp'))
  onTTFB(report('ttfb'))
}
