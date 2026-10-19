import { useEffect, useRef } from 'react'

/// Repeats `poll` with a fixed gap measured from the moment the previous poll
/// settled, not from the moment it started, and only while the tab is visible.
///
/// A plain interval assumes the response is faster than the period. When the
/// endpoint answers slower than that — the dashboard aggregate has measured
/// ~2.6s against a 2.5s period on a real library — a fixed interval issues the
/// next request while the previous one is still open. The requests then stack,
/// the endpoint never gets an idle moment, and a single transient failure lands
/// on a screen that is already showing data. Waiting for the settle keeps the
/// request rate at most one per `intervalMs` plus one response time.
///
/// A hidden tab skips its turn and re-arms rather than firing, so a forgotten
/// background tab does not keep the endpoint busy for nobody.
export function useSettledPolling(poll: () => unknown, intervalMs: number, enabled: boolean): void {
  const pollRef = useRef(poll)

  useEffect(() => {
    pollRef.current = poll
  }, [poll])

  useEffect(() => {
    if (!enabled) return undefined

    let cancelled = false
    let timer: ReturnType<typeof setTimeout> | undefined

    const scheduleNext = () => {
      timer = setTimeout(() => {
        if (document.visibilityState !== 'visible') {
          scheduleNext()
          return
        }
        void Promise.resolve(pollRef.current()).finally(() => {
          if (!cancelled) scheduleNext()
        })
      }, intervalMs)
    }

    scheduleNext()

    return () => {
      cancelled = true
      if (timer !== undefined) clearTimeout(timer)
    }
  }, [enabled, intervalMs])
}
