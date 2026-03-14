<script setup lang="ts">
import PageSection from 'src/components/shell/PageSection.vue'
import EmptyStateCard from 'src/components/state/EmptyStateCard.vue'
</script>

<template>
  <PageSection
    eyebrow="Operations"
    title="Diagnostics"
    description="Review failed jobs, degraded dependencies, retrieval anomalies, and the next remediation step for operators."
    status="Warning"
    status-label="Awaiting live signals"
  >
    <EmptyStateCard
      title="No diagnostics events yet"
      message="RustRAG has not reported any failed jobs, dependency alerts, or remediation tasks for this workspace yet."
      hint="If something looks wrong, check ingestion job history, provider connectivity, or retrieval traces from the linked pages. This view will surface incidents here as soon as backend diagnostics feeds are available."
    />
  </PageSection>
</template>
