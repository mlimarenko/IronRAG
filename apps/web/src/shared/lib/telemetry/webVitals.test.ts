import { afterEach, describe, expect, it, vi } from 'vitest'

const handlers = {
  cls: null as null | ((metric: { value: number; rating: string }) => void),
  fcp: null as null | ((metric: { value: number; rating: string }) => void),
  inp: null as null | ((metric: { value: number; rating: string }) => void),
  lcp: null as null | ((metric: { value: number; rating: string }) => void),
  ttfb: null as null | ((metric: { value: number; rating: string }) => void),
}

vi.mock('web-vitals', () => ({
  onCLS: (callback: typeof handlers.cls) => {
    handlers.cls = callback
  },
  onFCP: (callback: typeof handlers.fcp) => {
    handlers.fcp = callback
  },
  onINP: (callback: typeof handlers.inp) => {
    handlers.inp = callback
  },
  onLCP: (callback: typeof handlers.lcp) => {
    handlers.lcp = callback
  },
  onTTFB: (callback: typeof handlers.ttfb) => {
    handlers.ttfb = callback
  },
}))

describe('web vitals', () => {
  afterEach(() => {
    vi.resetModules()
  })

  it('reports each core vital as a named sample', async () => {
    const { observeWebVitals } = await import('./webVitals')
    const record = vi.fn()
    observeWebVitals(record)

    handlers.lcp?.({ value: 2500, rating: 'good' })
    handlers.cls?.({ value: 0.08, rating: 'needs-improvement' })
    handlers.ttfb?.({ value: 600, rating: 'good' })

    expect(record).toHaveBeenCalledWith({ name: 'lcp', value: 2500, rating: 'good' })
    expect(record).toHaveBeenCalledWith({ name: 'cls', value: 0.08, rating: 'needs-improvement' })
    expect(record).toHaveBeenCalledWith({ name: 'ttfb', value: 600, rating: 'good' })
  })
})
