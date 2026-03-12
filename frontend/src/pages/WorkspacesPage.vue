<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'

import { api, fetchWorkspaceGovernance, type WorkspaceGovernanceSummary } from 'src/boot/api'
import { buildOperatorState } from 'src/pages/support/view-state'

const workspaces = ref<{ id: string; slug: string; name: string; status: string }[]>([])
const governance = ref<WorkspaceGovernanceSummary | null>(null)
const loading = ref(true)
const errorMessage = ref<string | null>(null)

const viewState = computed(() => {
  if (loading.value) {
    return buildOperatorState(
      'loading',
      'Loading workspaces',
      'Fetching workspace governance state.',
    )
  }

  if (errorMessage.value) {
    return buildOperatorState('error', 'Workspace governance unavailable', errorMessage.value)
  }

  if (governance.value) {
    return buildOperatorState(
      governance.value.health_state === 'Healthy' ? 'success' : 'degraded',
      governance.value.name,
      `Projects: ${String(governance.value.projects)} • Providers: ${String(governance.value.provider_accounts)} • Models: ${String(governance.value.model_profiles)}`,
      {
        workspaceLabel: governance.value.slug,
      },
    )
  }

  return buildOperatorState(
    'empty',
    'No workspaces found',
    'Create the first workspace to start operating RustRAG.',
  )
})

onMounted(async () => {
  try {
    const { data } =
      await api.get<{ id: string; slug: string; name: string; status: string }[]>('/v1/workspaces')
    workspaces.value = data

    if (data.length > 0) {
      governance.value = await fetchWorkspaceGovernance(data[0].id)
    }
  } catch (error) {
    errorMessage.value = error instanceof Error ? error.message : 'Unknown workspace error'
  } finally {
    loading.value = false
  }
})
</script>

<template>
  <section>
    <h2>Workspaces</h2>
    <p>Manage isolated tenant-like containers for projects, provider accounts, and policies.</p>

    <div class="state-card" :data-state="viewState.state">
      <h3>{{ viewState.title }}</h3>
      <p>{{ viewState.message }}</p>
      <p v-if="governance">
        Usage events: {{ governance.usage.usage_events }} • Total tokens:
        {{ governance.usage.total_tokens }} • Estimated cost:
        {{ governance.usage.estimated_cost }}
      </p>
    </div>

    <ul v-if="workspaces.length > 0">
      <li v-for="workspace in workspaces" :key="workspace.id">
        {{ workspace.name }} ({{ workspace.slug }}) — {{ workspace.status }}
      </li>
    </ul>
  </section>
</template>

<style scoped>
.state-card {
  margin: 16px 0;
  padding: 16px;
  border-radius: 12px;
  border: 1px solid #d7dee7;
  background: #f8fbff;
}
</style>
