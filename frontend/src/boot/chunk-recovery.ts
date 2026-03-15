type VitePreloadFn = (...args: unknown[]) => Promise<unknown>

declare global {
  interface Window {
    __vitePreload?: VitePreloadFn
  }

  interface Event {
    payload?: unknown
  }
}

export default () => {
  let reloadScheduled = false

  function scheduleReload(reason: string) {
    if (reloadScheduled) {
      return
    }

    reloadScheduled = true
    console.warn(`[chunk-recovery] ${reason}; reloading application shell`)
    window.setTimeout(() => {
      window.location.reload()
    }, 50)
  }

  window.addEventListener('vite:preloadError', (event) => {
    console.error('[chunk-recovery] vite preload error', event.payload)
    event.preventDefault()
    scheduleReload('vite preload error')
  })

  window.addEventListener(
    'error',
    (event) => {
      const target = event.target
      if (target instanceof HTMLScriptElement || target instanceof HTMLLinkElement) {
        const source = target instanceof HTMLScriptElement ? target.src : target.href

        if (source.includes('/assets/')) {
          console.error('[chunk-recovery] asset load failure', source)
          scheduleReload(`asset load failure: ${source}`)
        }
      }
    },
    true,
  )

  const originalImport = window.__vitePreload
  if (typeof originalImport === 'function') {
    window.__vitePreload = (...args: unknown[]) => {
      const result = originalImport(...args)
      return Promise.resolve(result).catch((error: unknown) => {
        console.error('[chunk-recovery] dynamic import failure', error)
        scheduleReload('dynamic import failure')
        throw error
      })
    }
  }
}
