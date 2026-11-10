import { QueryClient } from '@tanstack/react-query'
import { describe, expect, it, vi } from 'vitest'

import {
  ApiError,
  ResponseContractError,
  getApiProblem,
  retryContractViolation,
  unwrap,
  validateQueryResponse,
} from './runtime'

describe('validated query responses', () => {
  it('keeps malformed data out of the cache and recovers with the original wire value', async () => {
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } })
    const queryKey = ['validated-response'] as const
    const valid = { items: ['entry'] }
    const request = vi.fn().mockResolvedValueOnce({ items: null }).mockResolvedValueOnce(valid)
    const validate = vi.fn((data: { items: unknown }) => {
      if (!Array.isArray(data.items)) throw new TypeError('items must be an array')
    })
    const options = { queryKey, queryFn: validateQueryResponse(request, validate) }

    const failure = await client.fetchQuery(options).catch((error: unknown) => error)
    expect(failure).toBeInstanceOf(ResponseContractError)
    expect(failure).toMatchObject({ message: 'items must be an array' })
    expect(client.getQueryData(queryKey)).toBeUndefined()
    expect(client.getQueryState(queryKey)?.status).toBe('error')

    await expect(client.fetchQuery(options)).resolves.toBe(valid)
    expect(client.getQueryData(queryKey)).toBe(valid)
    expect(request).toHaveBeenCalledTimes(2)
    expect(validate).toHaveBeenCalledTimes(2)
    client.clear()
  })

  it('retries a transport failure once but never a contract violation', () => {
    const contractError = new ResponseContractError('items must be an array')
    expect(retryContractViolation(0, contractError)).toBe(false)
    expect(retryContractViolation(0, new Error('Failed to fetch'))).toBe(true)
    expect(retryContractViolation(1, new Error('Failed to fetch'))).toBe(false)
  })

  it('keeps a typed endpoint contract error instead of wrapping it again', async () => {
    const typed = new ResponseContractError('queue contract')
    const options = {
      queryKey: ['typed-contract-error'] as const,
      queryFn: validateQueryResponse(vi.fn().mockResolvedValue({ items: [] }), () => {
        throw typed
      }),
    }
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } })

    await expect(client.fetchQuery(options)).rejects.toBe(typed)
    client.clear()
  })
})

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
