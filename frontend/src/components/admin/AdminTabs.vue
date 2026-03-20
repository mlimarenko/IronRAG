<script setup lang="ts">
import type { AdminTab, AdminTabAvailability, AdminTabCounts } from 'src/models/ui/admin'

const props = defineProps<{
  counts: AdminTabCounts
  availability: AdminTabAvailability
  activeTab: AdminTab
}>()

const emit = defineEmits<{
  change: [tab: AdminTab]
}>()

const tabs: { id: AdminTab; labelKey: string; count: keyof AdminTabCounts }[] = [
  { id: 'tokens', labelKey: 'admin.tabs.tokens', count: 'tokens' },
  { id: 'aiCatalog', labelKey: 'admin.tabs.aiCatalog', count: 'aiCatalog' },
  { id: 'pricing', labelKey: 'admin.tabs.pricing', count: 'pricing' },
  { id: 'audit', labelKey: 'admin.tabs.audit', count: 'audit' },
]
</script>

<template>
  <div class="rr-admin-tabs">
    <button
      v-for="tab in tabs.filter((item) => props.availability[item.id])"
      :key="tab.id"
      class="rr-admin-tabs__button"
      :class="{ 'is-active': props.activeTab === tab.id }"
      type="button"
      @click="emit('change', tab.id)"
    >
      <span>{{ $t(tab.labelKey) }}</span>
      <small>{{ props.counts[tab.count] }}</small>
    </button>
  </div>
</template>
