import type { QueryFunction, QueryKey, SkipToken } from '@tanstack/react-query'
import type { CreateClientConfig } from './generated/client.gen'

export const createClientConfig: CreateClientConfig = (config) => ({
  ...config,
  baseUrl: '',
  credentials: 'include',
})

/**
 * A response that violates the wire contract its endpoint owns. Retrying cannot
 * repair a deterministic malformed body, so it settles the query at once and
 * keeps the raw response out of the cache.
 */
export class ResponseContractError extends Error {
  constructor(message: string) {
    super(message)
    this.name = 'ResponseContractError'
  }
}

/**
 * Reject malformed responses in the request lifecycle, before they enter the
 * cache. A failure inside an endpoint-owned validator becomes a
 * {@link ResponseContractError} unless the validator already raised one, so a
 * caller can still distinguish its own typed contract error.
 */
export function validateQueryResponse<TData, TKey extends QueryKey>(
  queryFn: QueryFunction<TData, TKey> | SkipToken | undefined,
  validate: (data: TData) => unknown,
): QueryFunction<TData, TKey> {
  return async (context) => {
    if (typeof queryFn !== 'function') throw new TypeError('Query request function is missing')
    const data = await queryFn(context)
    try {
      validate(data)
    } catch (error) {
      if (error instanceof ResponseContractError) throw error
      throw new ResponseContractError(error instanceof Error ? error.message : String(error))
    }
    return data
  }
}

/**
 * One retry is for a transient transport failure. A contract violation is
 * deterministic: retrying only delays the operator-visible error and leaves the
 * query in a cancellable retry window where a later refetch can restart it.
 */
export function retryContractViolation(failureCount: number, error: unknown): boolean {
  if (error instanceof ResponseContractError) return false
  return failureCount < 1
}

export interface ApiErrorBody {
  [extension: string]: unknown
  detail?: string
  code?: string
  title?: string
  status?: number
  type?: string
  requestId?: string
}

function toApiErrorBody(error: unknown, status: number): ApiErrorBody {
  if (error instanceof Error) {
    return { detail: error.message, status }
  }
  if (error === null || Array.isArray(error) || typeof error !== 'object') {
    return { detail: String(error), status }
  }

  return { ...(error as Record<string, unknown>), status }
}

export class ApiError extends Error {
  public readonly code: string | undefined

  constructor(
    public status: number,
    public body: ApiErrorBody,
  ) {
    super(body.detail || body.title || `API error ${status}`)
    this.name = 'ApiError'
    this.code = body.code
  }
}

export type ApiProblem = Readonly<{
  body: ApiErrorBody
  code: string | undefined
  status: number
}>

export function getApiProblem(error: unknown): ApiProblem | null {
  return error instanceof ApiError
    ? { body: error.body, code: error.code, status: error.status }
    : null
}

/**
 * Convert a generated-client result envelope into the thrown ApiError used by
 * imperative API facades.
 */
export function unwrap<T>(result: {
  data?: T | undefined
  error?: unknown
  response?: Response | undefined
}): T {
  if (result.error !== undefined && result.error !== null) {
    const status = result.response?.status ?? 0
    throw new ApiError(status, toApiErrorBody(result.error, status))
  }
  if (result.data === undefined) {
    return undefined as T
  }
  return result.data
}
