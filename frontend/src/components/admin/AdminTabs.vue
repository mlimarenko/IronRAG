<script setup lang="ts">
import type { AdminOverviewResponse, AdminTab } from 'src/models/ui/admin'

const props = defineProps<{
  overview: AdminOverviewResponse
  activeTab: AdminTab
}>()

const emit = defineEmits<{
  change: [tab: AdminTab]
}>()

const tabs: { id: AdminTab; labelKey: string; count: keyof AdminOverviewResponse['counts'] }[] = [
  { id: 'api_tokens', labelKey: 'admin.tabs.apiTokens', count: 'apiTokens' },
  { id: 'members', labelKey: 'admin.tabs.members', count: 'members' },
  { id: 'library_access', labelKey: 'admin.tabs.libraryAccess', count: 'libraryAccess' },
  { id: 'settings', labelKey: 'admin.tabs.settings', count: 'settings' },
]
</script>

<template>
  <div class="rr-admin-tabs">
    <button
      v-for="tab in tabs"
      :key="tab.id"
      class="rr-admin-tabs__button"
      :class="{ 'is-active': props.activeTab === tab.id }"
      type="button"
      @click="emit('change', tab.id)"
    >
      <span>{{ $t(tab.labelKey) }}</span>
      <small>{{ props.overview.counts[tab.count] }}</small>
    </button>
  </div>
</template>
