import { afterEach, describe, expect, it, vi } from 'vitest'

import { installGlobalErrorHandlers } from './errors'

describe('global browser errors', () => {
  let cleanup: (() => void) | null = null

  afterEach(() => {
    cleanup?.()
    cleanup = null
  })

  it('captures uncaught errors with a redacted message', () => {
    const onError = vi.fn()
    cleanup = installGlobalErrorHandlers(onError)

    window.dispatchEvent(new ErrorEvent('error', { error: new TypeError('failed for a@b.co') }))

    expect(onError).toHaveBeenCalledWith(
      expect.objectContaining({
        source: 'window.error',
        type: 'TypeError',
        message: 'failed for [redacted-email]',
      }),
    )
  })

  it('captures unhandled promise rejections', () => {
    const onError = vi.fn()
    cleanup = installGlobalErrorHandlers(onError)

    const event = new Event('unhandledrejection') as PromiseRejectionEvent
    Object.defineProperty(event, 'reason', { value: new Error('rejected') })
    window.dispatchEvent(event)

    expect(onError).toHaveBeenCalledWith(
      expect.objectContaining({ source: 'unhandledrejection', message: 'rejected' }),
    )
  })

  it('ignores resource errors without an exception', () => {
    const onError = vi.fn()
    cleanup = installGlobalErrorHandlers(onError)

    window.dispatchEvent(new ErrorEvent('error', { message: 'missing image' }))

    expect(onError).not.toHaveBeenCalled()
  })

  it('removes its listeners on cleanup', () => {
    const onError = vi.fn()
    const removeListener = vi.spyOn(window, 'removeEventListener')
    cleanup = installGlobalErrorHandlers(onError)
    cleanup()
    cleanup = null

    expect(removeListener).toHaveBeenCalledWith('error', expect.any(Function))
    expect(removeListener).toHaveBeenCalledWith('unhandledrejection', expect.any(Function))
    expect(onError).not.toHaveBeenCalled()
  })
})
