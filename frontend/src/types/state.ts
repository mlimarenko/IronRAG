export type AsyncStatus = 'idle' | 'loading' | 'success' | 'error'

export interface AsyncState<T, E = string> {
  data: T
  status: AsyncStatus
  error: E | null
  lastLoadedAt: string | null
}

export interface SelectOption {
  label: string
  value: string
}

export interface EntityReference {
  id: string
  label: string
}

export function createAsyncState<T, E = string>(data: T): AsyncState<T, E> {
  return {
    data,
    status: 'idle',
    error: null,
    lastLoadedAt: null,
  }
}
