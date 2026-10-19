import { useCallback, useLayoutEffect, useRef } from 'react'
import type { SetStateAction } from 'react'
import { useSearchParams } from 'react-router-dom'

/** Queue URL updates against the last requested URL, not the last committed render. */
export function useQueuedSearchParams() {
  const [params, navigate] = useSearchParams()
  const pending = useRef(params)
  useLayoutEffect(() => {
    pending.current = params
  }, [params])
  const setParams = useCallback(
    (update: SetStateAction<URLSearchParams>) => {
      const current = new URLSearchParams(pending.current)
      const next = typeof update === 'function' ? update(current) : update
      if (next.toString() === pending.current.toString()) return
      pending.current = new URLSearchParams(next)
      navigate(next, { replace: true })
    },
    [navigate],
  )
  return [params, setParams] as const
}
