import type { CreateClientConfig } from './generated/client.gen'

export const createClientConfig: CreateClientConfig = (config) => ({
  ...config,
  baseUrl: '',
  credentials: 'include',
})

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
