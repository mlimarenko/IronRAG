<script setup lang="ts">
import { useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import PageHeader from 'src/components/design-system/PageHeader.vue'
import type { DashboardPrimaryAction } from 'src/models/ui/dashboard'

const props = defineProps<{
  narrative: string
  actions: DashboardPrimaryAction[]
}>()

const { t } = useI18n()
const router = useRouter()

async function handleAction(action: DashboardPrimaryAction) {
  if (!action.route) {
    return
  }
  await router.push(action.route)
}
</script>

<template>
  <PageHeader
    compact
    :eyebrow="t('dashboard.eyebrow')"
    :title="t('dashboard.title')"
    :subtitle="props.narrative"
  >
    <template #actions>
      <button
        v-for="(action, index) in props.actions"
        :key="action.key"
        type="button"
        class="rr-button"
        :class="index === 0 ? 'rr-button--primary' : 'rr-button--ghost'"
        @click="handleAction(action)"
      >
        {{ action.label }}
      </button>
    </template>
  </PageHeader>
</template>
