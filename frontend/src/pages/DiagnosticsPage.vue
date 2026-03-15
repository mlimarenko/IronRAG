<script setup lang="ts">
import PageSection from 'src/components/shell/PageSection.vue'
import EmptyStateCard from 'src/components/state/EmptyStateCard.vue'
</script>

<template>
  <PageSection
    eyebrow="Advanced"
    title="Diagnostics"
    description="Advanced troubleshooting details stay here when you need to inspect incidents outside the main flow."
    status="Focused"
    status-label="Advanced troubleshooting"
  >
    <EmptyStateCard
      title="No troubleshooting signals yet"
      message="RustRAG has not reported any incidents for this workspace yet."
      hint="If something looks wrong, open this advanced page to inspect incidents as backend troubleshooting feeds become available."
    />
  </PageSection>
</template>
