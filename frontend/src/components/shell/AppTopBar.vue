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
const {
  activeLibrary,
  activeWorkspace,
  canCreateLibrary,
  canCreateWorkspace,
  currentUser,
  libraries,
  workspaces,
} = storeToRefs(shellStore)

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
      v-if="currentUser"
      class="rr-topbar__meta"
    >
      <ContextSelector
        :label="$t('shell.workspace')"
        :selected-id="activeWorkspace?.id ?? ''"
        :options="workspaces"
        :disabled="!workspaces.length"
        :placeholder="$t('shell.noWorkspaces')"
        :can-create="canCreateWorkspace"
        :create-label="$t('shell.createWorkspace')"
        @change="shellStore.switchWorkspace"
        @create="shellStore.showCreateWorkspace = true"
      />
      <ContextSelector
        v-if="activeWorkspace || libraries.length || canCreateLibrary"
        :label="$t('shell.library')"
        :selected-id="activeLibrary?.id ?? ''"
        :options="libraries"
        :disabled="!activeWorkspace || !libraries.length"
        :placeholder="activeWorkspace ? $t('shell.noLibraries') : $t('shell.selectWorkspaceFirst')"
        :can-create="canCreateLibrary"
        :create-label="$t('shell.createLibrary')"
        @change="shellStore.switchLibrary"
        @create="shellStore.showCreateLibrary = true"
      />
      <LocaleSwitcher
        :locale="sessionStore.locale"
        @change="shellStore.switchLocale"
      />
      <UserMenu
        :initials="currentUser.initials"
        :display-name="currentUser.displayName"
        :access-label="currentUser.accessLabel"
        @logout="logout"
      />
    </div>
  </header>
</template>
