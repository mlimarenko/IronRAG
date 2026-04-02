<script setup lang="ts">
import { storeToRefs } from 'pinia'
import type { LibraryOption, WorkspaceOption } from 'src/models/ui/shell'
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

function onDeleteWorkspace(option: WorkspaceOption | LibraryOption) {
  shellStore.requestDeleteWorkspace(option as WorkspaceOption)
}

function onDeleteLibrary(option: WorkspaceOption | LibraryOption) {
  shellStore.requestDeleteLibrary(option as LibraryOption)
}

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

      <div v-if="currentUser" class="rr-shellbar__controls">
        <div class="rr-shellbar__context">
          <ContextSelector
            class="rr-shellbar__selector rr-shellbar__selector--workspace"
            :label="$t('shell.workspace')"
            :selected-id="activeWorkspace?.id ?? ''"
            :options="workspaces"
            compact
            :disabled="!workspaces.length"
            :placeholder="$t('shell.noWorkspaces')"
            :can-create="canCreateWorkspace"
            :create-label="$t('shell.createWorkspace')"
            :can-delete="canCreateWorkspace"
            @change="shellStore.switchWorkspace"
            @create="shellStore.showCreateWorkspace = true"
            @delete="onDeleteWorkspace"
          />
          <ContextSelector
            v-if="activeWorkspace || libraries.length || canCreateLibrary"
            class="rr-shellbar__selector rr-shellbar__selector--library"
            :label="$t('shell.library')"
            :selected-id="activeLibrary?.id ?? ''"
            :options="libraries"
            compact
            :disabled="!activeWorkspace || !libraries.length"
            :placeholder="
              activeWorkspace ? $t('shell.noLibraries') : $t('shell.selectWorkspaceFirst')
            "
            :can-create="canCreateLibrary"
            :create-label="$t('shell.createLibrary')"
            :can-delete="canCreateLibrary"
            @change="shellStore.switchLibrary"
            @create="shellStore.showCreateLibrary = true"
            @delete="onDeleteLibrary"
          />
        </div>

        <div class="rr-shellbar__account">
          <LocaleSwitcher :locale="sessionStore.locale" @change="shellStore.switchLocale" />
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

<!-- styles consolidated in app.scss -->
