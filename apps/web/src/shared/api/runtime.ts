import type { CreateClientConfig } from './generated/client.gen'

export const createClientConfig: CreateClientConfig = (config) => ({
  ...config,
  baseUrl: '',
  credentials: 'include',
})

export interface ApiErrorBody {
  error?: string
  message?: string
  [key: string]: unknown
}

function toApiErrorBody(error: unknown): ApiErrorBody {
  if (error instanceof Error) {
    return { error: error.message }
  }
  if (error !== null && !Array.isArray(error) && typeof error === 'object') {
    return error
  }
  return { error: String(error) }
}

export class ApiError extends Error {
  constructor(
    public status: number,
    public body: ApiErrorBody,
  ) {
    super(body?.error || body?.message || `API error ${status}`)
    this.name = 'ApiError'
  }
}

/**
 * Convert a hey-api result envelope into the canonical thrown ApiError used by
 * imperative API facades.
 */
export function unwrap<T>(result: {
  data?: T | undefined
  error?: unknown
  response?: Response | undefined
}): T {
  if (result.error !== undefined && result.error !== null) {
    const status = result.response?.status ?? 0
    throw new ApiError(status, toApiErrorBody(result.error))
  }
  if (result.data === undefined) {
    return undefined as T
  }
  return result.data
}
