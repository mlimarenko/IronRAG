import type { Router } from 'vue-router'
import { i18n } from 'src/lib/i18n'
import { pinia } from 'src/lib/pinia'
import { useSessionStore } from 'src/stores/session'
import { useShellStore } from 'src/stores/shell'

export function installRouteGuards(router: Router) {
  router.beforeEach(async (to) => {
    const sessionStore = useSessionStore(pinia)
    const shellStore = useShellStore(pinia)

    if (sessionStore.status === 'idle') {
      await sessionStore.restoreSession()
      i18n.global.locale.value = sessionStore.locale
      if (sessionStore.isAuthenticated) {
        await shellStore.loadContext()
      }
    }

    if (to.meta.requiresAuth && !sessionStore.isAuthenticated) {
      return '/login'
    }

    if (to.meta.guestOnly && sessionStore.isAuthenticated) {
      return '/'
    }

    if (to.meta.requiresAdmin && !shellStore.adminEnabled) {
      return '/'
    }

    return true
  })
}
