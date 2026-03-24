<script setup lang="ts">
import { computed, onMounted } from 'vue'
import { useRoute } from 'vue-router'
import AppTopBar from 'src/components/shell/AppTopBar.vue'
import CreateLibraryDialog from 'src/components/shell/CreateLibraryDialog.vue'
import CreateWorkspaceDialog from 'src/components/shell/CreateWorkspaceDialog.vue'
import { useShellStore } from 'src/stores/shell'

const shellStore = useShellStore()
const route = useRoute()

const routeWidthClass = computed(() => {
  const mode = (route.meta as Record<string, unknown>).widthMode as string | undefined
  if (mode === 'wide') return 'rr-route-width--wide'
  if (mode === 'full') return 'rr-route-width--full'
  return 'rr-route-width--default'
})

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
    <main
      class="rr-app-shell__content"
      :class="routeWidthClass"
    >
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

<style lang="scss">
.rr-route-width--default {
  max-width: var(--rr-route-width-default);
  margin-inline: auto;
  padding-inline: var(--rr-sp-4);
}

.rr-route-width--wide {
  max-width: var(--rr-route-width-wide);
  margin-inline: auto;
  padding-inline: var(--rr-sp-4);
}

.rr-route-width--full {
  max-width: var(--rr-route-width-full);
}
</style>
