<script setup lang="ts">
import { computed, onMounted } from 'vue'
import { useRoute } from 'vue-router'
import PageFrame from 'src/components/design-system/PageFrame.vue'
import AppTopBar from 'src/components/shell/AppTopBar.vue'
import CreateLibraryDialog from 'src/components/shell/CreateLibraryDialog.vue'
import CreateWorkspaceDialog from 'src/components/shell/CreateWorkspaceDialog.vue'
import DeleteConfirmDialog from 'src/components/shell/DeleteConfirmDialog.vue'
import { useShellStore } from 'src/stores/shell'
import { useI18n } from 'vue-i18n'

const { t } = useI18n()
const shellStore = useShellStore()
const route = useRoute()

const routeWidthMode = computed(() => {
  const mode = (route.meta as Record<string, unknown>).widthMode as string | undefined
  if (mode === 'wide' || mode === 'full') {
    return mode
  }
  return 'default'
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
      role="alert"
    >
      {{ shellStore.error }}
    </p>
    <main
      class="rr-app-shell__content"
    >
      <PageFrame :width-mode="routeWidthMode">
        <router-view />
      </PageFrame>
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
    <DeleteConfirmDialog
      :open="shellStore.showDeleteWorkspace"
      :title="t('shell.deleteWorkspace')"
      :target-name="shellStore.deleteWorkspaceTarget?.name ?? ''"
      :warning="t('shell.deleteWorkspaceWarning')"
      @close="shellStore.cancelDeleteWorkspace()"
      @confirm="shellStore.confirmDeleteWorkspace()"
    />
    <DeleteConfirmDialog
      :open="shellStore.showDeleteLibrary"
      :title="t('shell.deleteLibrary')"
      :target-name="shellStore.deleteLibraryTarget?.name ?? ''"
      :warning="t('shell.deleteLibraryWarning')"
      @close="shellStore.cancelDeleteLibrary()"
      @confirm="shellStore.confirmDeleteLibrary()"
    />
  </div>
</template>
