<script setup lang="ts">
import { onMounted } from 'vue'
import AppTopBar from 'src/components/shell/AppTopBar.vue'
import CreateLibraryDialog from 'src/components/shell/CreateLibraryDialog.vue'
import CreateWorkspaceDialog from 'src/components/shell/CreateWorkspaceDialog.vue'
import { useShellStore } from 'src/stores/shell'

const shellStore = useShellStore()

onMounted(async () => {
  if (!shellStore.context) {
    await shellStore.loadContext()
  }
})
</script>

<template>
  <div class="rr-app-shell">
    <AppTopBar />
    <p
      v-if="shellStore.error"
      class="rr-shell-error-banner"
    >
      {{ shellStore.error }}
    </p>
    <main class="rr-app-shell__content">
      <router-view />
    </main>
    <CreateWorkspaceDialog
      :open="shellStore.showCreateWorkspace"
      @close="shellStore.showCreateWorkspace = false"
      @submit="shellStore.submitWorkspace"
    />
    <CreateLibraryDialog
      :open="shellStore.showCreateLibrary"
      @close="shellStore.showCreateLibrary = false"
      @submit="shellStore.submitLibrary"
    />
  </div>
</template>
