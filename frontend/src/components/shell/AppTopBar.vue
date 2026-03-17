<script setup lang="ts">
import { storeToRefs } from 'pinia'
import router from 'src/router'
import { useSessionStore } from 'src/stores/session'
import { useShellStore } from 'src/stores/shell'
import AppBrand from './AppBrand.vue'
import AppNavTabs from './AppNavTabs.vue'
import ContextSelector from './ContextSelector.vue'
import LocaleSwitcher from './LocaleSwitcher.vue'
import UserMenu from './UserMenu.vue'

const sessionStore = useSessionStore()
const shellStore = useShellStore()
const { context } = storeToRefs(shellStore)

async function logout() {
  await sessionStore.logout()
  shellStore.clearContext()
  await router.push('/login')
}
</script>

<template>
  <header class="rr-topbar">
    <div class="rr-topbar__primary">
      <AppBrand />
      <AppNavTabs />
    </div>

    <div
      v-if="context"
      class="rr-topbar__meta"
    >
      <ContextSelector
        :label="$t('shell.workspace')"
        :selected-id="context.activeWorkspace.id"
        :options="context.workspaces"
        @change="shellStore.switchWorkspace"
        @create="shellStore.showCreateWorkspace = true"
      />
      <ContextSelector
        :label="$t('shell.library')"
        :selected-id="context.activeLibrary.id"
        :options="context.libraries"
        @change="shellStore.switchLibrary"
        @create="shellStore.showCreateLibrary = true"
      />
      <LocaleSwitcher
        :locale="sessionStore.locale"
        @change="shellStore.switchLocale"
      />
      <UserMenu
        :initials="context.currentUser.initials"
        :display-name="context.currentUser.displayName"
        :role-label="context.currentUser.roleLabel"
        @logout="logout"
      />
    </div>
  </header>
</template>
