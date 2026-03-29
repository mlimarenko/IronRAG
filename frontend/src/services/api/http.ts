import axios from 'axios'

export interface ApiErrorPayload {
  error?: string
  errorKind?: string | null
  details?: unknown
  requestId?: string | null
}

export class ApiClientError extends Error {
  readonly statusCode: number | null
  readonly errorKind: string | null
  readonly details: unknown
  readonly requestId: string | null

  constructor(
    message: string,
    statusCode: number | null,
    errorKind: string | null = null,
    details: unknown = null,
    requestId: string | null = null,
  ) {
    super(message)
    this.name = 'ApiClientError'
    this.statusCode = statusCode
    this.errorKind = errorKind
    this.details = details
    this.requestId = requestId
  }
}

export const apiBasePath = import.meta.env.VITE_API_BASE_URL ?? '/v1'

export function resolveApiPath(path: string): string {
  const normalizedBase = apiBasePath.endsWith('/') ? apiBasePath.slice(0, -1) : apiBasePath
  const normalizedPath = path.startsWith('/') ? path : `/${path}`
  return `${normalizedBase}${normalizedPath}`
}

export const apiHttp = axios.create({
  baseURL: apiBasePath,
  withCredentials: true,
})

function normalizeApiErrorMessage(message: string): string {
  const knownPrefixes = ['bad request: ', 'conflict: ', 'not found: ']

  for (const prefix of knownPrefixes) {
    if (message.startsWith(prefix)) {
      return message.slice(prefix.length).trim()
    }
  }

  return message
}

export async function unwrap<T>(promise: Promise<{ data: T }>): Promise<T> {
  try {
    const response = await promise
    return response.data
  } catch (error) {
    if (axios.isAxiosError<ApiErrorPayload>(error)) {
      const response = error.response
      const payload = response?.data
      throw new ApiClientError(
        response ? normalizeApiErrorMessage(payload?.error ?? error.message) : error.message,
        response ? response.status : null,
        payload?.errorKind ?? null,
        payload?.details ?? null,
        payload?.requestId ?? null,
      )
    }
    throw error
  }
}
