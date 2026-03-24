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
  <header class="rr-shellbar">
    <div class="rr-shellbar__frame">
      <div class="rr-shellbar__nav">
        <AppBrand />
        <AppNavTabs />
      </div>

      <div
        v-if="currentUser"
        class="rr-shellbar__controls"
      >
        <div class="rr-shellbar__context">
          <ContextSelector
            :label="$t('shell.workspace')"
            :selected-id="activeWorkspace?.id ?? ''"
            :options="workspaces"
            compact
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
            compact
            :disabled="!activeWorkspace || !libraries.length"
            :placeholder="activeWorkspace ? $t('shell.noLibraries') : $t('shell.selectWorkspaceFirst')"
            :can-create="canCreateLibrary"
            :create-label="$t('shell.createLibrary')"
            @change="shellStore.switchLibrary"
            @create="shellStore.showCreateLibrary = true"
          />
        </div>

        <div class="rr-shellbar__account">
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
      </div>
    </div>
  </header>
</template>

<style scoped lang="scss">
.rr-shellbar {
  border-bottom: 1px solid rgba(226, 232, 240, 0.9);
}

.rr-shellbar__frame {
  width: min(1760px, calc(100% - 8px));
}

.rr-shellbar__nav {
  gap: 14px;
}

.rr-shellbar :deep(.rr-nav-tabs) {
  padding: 4px;
  border: 1px solid rgba(203, 213, 225, 0.88);
  border-radius: 999px;
  background: rgba(255, 255, 255, 0.88);
}

.rr-shellbar :deep(.rr-nav-tabs__link) {
  min-height: 34px;
  padding: 0 12px;
  border-radius: 999px;
  font-size: 12px;
}

.rr-shellbar :deep(.rr-nav-tabs__link.is-active) {
  box-shadow: 0 6px 14px rgba(15, 23, 42, 0.08);
}
</style>
