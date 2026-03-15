<script setup lang="ts">
import { useRouter } from 'vue-router'

import PageSection from 'src/components/shell/PageSection.vue'
import EmptyStateCard from 'src/components/state/EmptyStateCard.vue'

const router = useRouter()
</script>

<template>
  <PageSection
    eyebrow="Advanced"
    title="Setup archive"
    description="This legacy setup surface is quarantined outside the main flow and kept only as an advanced reference during migration."
    status="Focused"
    status-label="Quarantined"
  >
    <EmptyStateCard
      title="Use Documents and Ask instead"
      message="Default workspace and library context now belongs in navigation. Upload from Documents, then move into Ask when content is ready."
      hint="Only return here if a migration or support check specifically points to the archived setup surface."
    />

    <div class="onboarding-archive__actions">
      <button
        type="button"
        class="rr-button rr-button--primary"
        @click="router.push('/documents')"
      >
        Open Documents
      </button>
      <button
        type="button"
        class="rr-button rr-button--secondary"
        @click="router.push('/advanced/context')"
      >
        Open Advanced context
      </button>
    </div>
  </PageSection>
</template>

<style scoped>
.onboarding-archive__actions {
  display: flex;
  flex-wrap: wrap;
  gap: var(--rr-space-3);
}
</style>
