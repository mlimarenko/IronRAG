import { describe, expect, it } from 'vitest'

import { ApiError } from '@/shared/api'
import { isInvalidCursorError } from './documentsQueryState'

describe('isInvalidCursorError', () => {
  it('recognizes facade and generated-client HTTP 400 errors for an active cursor', () => {
    expect(isInvalidCursorError(new ApiError(400, { detail: 'invalid cursor' }), 'next')).toBe(true)
    expect(isInvalidCursorError({ status: 400, detail: 'invalid cursor' }, 'next')).toBe(true)
    expect(isInvalidCursorError({ response: { status: 400 } }, 'next')).toBe(true)
  })

  it('does not reset the first page or non-400 failures', () => {
    expect(isInvalidCursorError({ status: 400 }, null)).toBe(false)
    expect(isInvalidCursorError({ status: 503 }, 'next')).toBe(false)
  })
})
