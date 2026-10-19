import { useSyncExternalStore } from 'react'

function subscribe(onChange: () => void): () => void {
  window.addEventListener('resize', onChange)
  return () => window.removeEventListener('resize', onChange)
}

/**
 * The viewport width, tracked through the store hook so a resize cannot leave a
 * layout reading a width from a render that is no longer current.
 *
 * The server snapshot is the widest common desktop; nothing here renders on a
 * server today, and a zero would make every width-derived bound collapse.
 */
export function useWindowWidth(): number {
  return useSyncExternalStore(
    subscribe,
    () => window.innerWidth,
    () => 1920,
  )
}
