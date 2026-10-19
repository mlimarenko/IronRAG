import { describe, expect, it } from 'vitest'

import { ApiError, getApiProblem, unwrap } from './runtime'

describe('unwrap', () => {
  it('preserves an Error message from a generated-client failure', () => {
    expect(() => unwrap({ error: new Error('transport unavailable') })).toThrow(
      'transport unavailable',
    )
  })

  it('preserves the complete generated-client problem document', () => {
    try {
      unwrap({
        error: {
          detail: 'request rejected',
          code: 'invalid_request',
          title: 'Bad Request',
          type: 'https://example.test/problems/invalid-request',
          requestId: 'request-1',
          existingDocumentId: 'document-1',
          status: 400,
        },
        response: new Response(null, { status: 400 }),
      })
      throw new Error('expected unwrap to throw')
    } catch (error) {
      expect(error).toBeInstanceOf(ApiError)
      expect((error as ApiError).status).toBe(400)
      expect((error as ApiError).body).toEqual({
        detail: 'request rejected',
        code: 'invalid_request',
        title: 'Bad Request',
        type: 'https://example.test/problems/invalid-request',
        requestId: 'request-1',
        existingDocumentId: 'document-1',
        status: 400,
      })
      expect((error as ApiError).code).toBe('invalid_request')
      expect(getApiProblem(error)).toMatchObject({
        status: 400,
        code: 'invalid_request',
        body: { existingDocumentId: 'document-1' },
      })
    }
  })

  it('does not use legacy error or message fields', () => {
    try {
      unwrap({
        error: { error: 'legacy error', message: 'legacy message' },
        response: new Response(null, { status: 400 }),
      })
      throw new Error('expected unwrap to throw')
    } catch (error) {
      expect(error).toBeInstanceOf(ApiError)
      expect((error as ApiError).message).toBe('API error 400')
      expect((error as ApiError).body).toEqual({
        error: 'legacy error',
        message: 'legacy message',
        status: 400,
      })
    }
  })
})
