import axios from 'axios'

export class ApiClientError extends Error {
  readonly statusCode: number | null

  constructor(message: string, statusCode: number | null) {
    super(message)
    this.name = 'ApiClientError'
    this.statusCode = statusCode
  }
}

export const apiHttp = axios.create({
  baseURL: import.meta.env.VITE_API_BASE_URL ?? '/v1',
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
    if (axios.isAxiosError<{ error?: string }>(error)) {
      const response = error.response
      throw new ApiClientError(
        response ? normalizeApiErrorMessage(response.data.error ?? error.message) : error.message,
        response ? response.status : null,
      )
    }
    throw error
  }
}
