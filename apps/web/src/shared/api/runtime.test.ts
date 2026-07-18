import { describe, expect, it } from 'vitest'

import { ApiError, unwrap } from './runtime'

describe('unwrap', () => {
  it('preserves an Error message from a generated-client failure', () => {
    expect(() => unwrap({ error: new Error('transport unavailable') })).toThrow(
      'transport unavailable',
    )
  })

  it('preserves structured API error fields', () => {
    try {
      unwrap({
        error: { message: 'request rejected' },
        response: new Response(null, { status: 400 }),
      })
      throw new Error('expected unwrap to throw')
    } catch (error) {
      expect(error).toBeInstanceOf(ApiError)
      expect((error as ApiError).status).toBe(400)
      expect((error as ApiError).body).toEqual({ message: 'request rejected' })
    }
  })
})
