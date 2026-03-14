<script setup lang="ts">
import { onMounted, ref } from 'vue'

import { api, type WorkspaceGovernanceSummary } from 'src/boot/api'

const workspaces = ref<{ id: string; slug: string; name: string; status: string }[]>([])
const governance = ref<WorkspaceGovernanceSummary | null>(null)
const loading = ref(true)
const infoMessage = ref<string | null>(null)
const errorMessage = ref<string | null>(null)

function extractErrorMessage(error: unknown): string {
  return error instanceof Error ? error.message : 'Unknown workspace error'
}

function isUnauthorizedMessage(message: string): boolean {
  const normalized = message.toLowerCase()
  return normalized.includes('401') || normalized.includes('unauthorized') || normalized.includes('authorization')
}

onMounted(async () => {
  try {
    const { data } =
      await api.get<{ id: string; slug: string; name: string; status: string }[]>('/workspaces')
    workspaces.value = data

    if (data.length === 0) {
      infoMessage.value = 'No workspaces created yet. Create the first workspace to unlock governance views.'
      return
    }

    try {
      const response = await api.get<WorkspaceGovernanceSummary>(`/workspaces/${data[0].id}/governance`)
      governance.value = response.data
    } catch (error) {
      const message = extractErrorMessage(error)
      if (isUnauthorizedMessage(message)) {
        infoMessage.value = 'Workspace list is available, but governance details require an authorized API token.'
      } else {
        errorMessage.value = message
      }
    }
  } catch (error) {
    errorMessage.value = extractErrorMessage(error)
  } finally {
    loading.value = false
  }
})
</script>

<template>
  <section>
    <h2>Workspaces</h2>
    <p>Manage isolated tenant-like containers for projects, provider accounts, and policies.</p>

    <p v-if="loading">Loading workspaces…</p>
    <p v-else-if="errorMessage">{{ errorMessage }}</p>
    <div
      v-else
      class="state-card"
    >
      <template v-if="governance">
        <h3>{{ governance.name }}</h3>
        <p>
          Projects: {{ governance.projects }} • Providers: {{ governance.provider_accounts }} •
          Models: {{ governance.model_profiles }}
        </p>
        <p>
          Usage events: {{ governance.usage.usage_events }} • Total tokens:
          {{ governance.usage.total_tokens }} • Estimated cost:
          {{ governance.usage.estimated_cost }}
        </p>
      </template>

      <template v-else>
        <h3>{{ workspaces.length > 0 ? 'Workspace list ready' : 'No workspaces found' }}</h3>
        <p>
          {{
            infoMessage ??
            'Create the first workspace to start operating RustRAG.'
          }}
        </p>
      </template>
    </div>

    <ul v-if="workspaces.length > 0">
      <li
        v-for="workspace in workspaces"
        :key="workspace.id"
      >
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
