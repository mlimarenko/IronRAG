import type { ReactNode } from 'react'
import { AlertTriangle, Loader2 } from 'lucide-react'

import { Alert, AlertDescription, AlertTitle } from '@/shared/components/ui/alert'
import { safeErrorMessage } from '@/shared/lib/errorMessage'

type DataStateQuery<T> = {
  isLoading: boolean
  error: unknown
  data: T | undefined
}

type DataStateProps<T> = {
  query: DataStateQuery<T>
  loading?: ReactNode
  errorRender?: ReactNode | ((err: unknown) => ReactNode)
  emptyCheck?: (data: T) => boolean
  emptyRender?: ReactNode
  children: (data: T) => ReactNode
}

function DefaultLoading() {
  return (
    <div className="flex min-h-24 items-center justify-center">
      <Loader2 className="h-5 w-5 animate-spin text-primary/70" aria-label="Loading" />
    </div>
  )
}

function DefaultError({ error }: Readonly<{ error: unknown }>) {
  return (
    <Alert variant="destructive" className="my-2">
      <AlertTriangle className="h-4 w-4" />
      <AlertTitle>Unable to load data</AlertTitle>
      <AlertDescription>{safeErrorMessage(error, 'The request failed.')}</AlertDescription>
    </Alert>
  )
}

function defaultEmptyCheck<T>(data: T): boolean {
  return Array.isArray(data) && data.length === 0
}

export function DataState<T>({
  query,
  loading,
  errorRender,
  emptyCheck,
  emptyRender,
  children,
}: Readonly<DataStateProps<T>>) {
  if (query.isLoading) {
    return <>{loading ?? <DefaultLoading />}</>
  }

  if (query.error) {
    if (typeof errorRender === 'function') return <>{errorRender(query.error)}</>
    return <>{errorRender ?? <DefaultError error={query.error} />}</>
  }

  if (query.data === undefined) {
    return <>{emptyRender ?? null}</>
  }

  if (emptyRender !== undefined && (emptyCheck?.(query.data) ?? defaultEmptyCheck(query.data))) {
    return <>{emptyRender ?? null}</>
  }

  return <>{children(query.data)}</>
}
