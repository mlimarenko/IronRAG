<script setup lang="ts">
import { onMounted, ref } from 'vue'

import { api, fetchProviderGovernance, type ProviderGovernanceSummary } from 'src/boot/api'

const workspaceId = ref<string | null>(null)
const governance = ref<ProviderGovernanceSummary | null>(null)
const infoMessage = ref<string | null>(null)
const errorMessage = ref<string | null>(null)
const loading = ref(true)

function extractErrorMessage(error: unknown): string {
  return error instanceof Error ? error.message : 'Unknown provider error'
}

function isUnauthorizedMessage(message: string): boolean {
  const normalized = message.toLowerCase()
  return normalized.includes('401') || normalized.includes('unauthorized') || normalized.includes('authorization')
}

onMounted(async () => {
  try {
    const { data } = await api.get<{ id: string }[]>('/workspaces')
    workspaceId.value = data[0]?.id ?? null

    if (!workspaceId.value) {
      infoMessage.value = 'No workspace yet. Create a workspace before configuring provider accounts.'
      return
    }

    try {
      governance.value = await fetchProviderGovernance(workspaceId.value)
    } catch (error) {
      const message = extractErrorMessage(error)
      if (isUnauthorizedMessage(message)) {
        infoMessage.value = 'Provider governance requires an authorized API token. Workspace discovery is working.'
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
    <h2>Providers</h2>
    <p>Configure OpenAI, DeepSeek, and compatible provider accounts plus model profiles.</p>

    <p v-if="loading">Loading provider surfaces…</p>
    <p v-else-if="errorMessage">{{ errorMessage }}</p>
    <div v-else-if="governance">
      <p v-if="governance.warning">{{ governance.warning }}</p>

      <h3>Provider accounts</h3>
      <ul>
        <li
          v-for="provider in governance.provider_accounts"
          :key="provider.id"
        >
          {{ provider.label }} — {{ provider.provider_kind }} — {{ provider.status }}
        </li>
      </ul>

      <h3>Model profiles</h3>
      <ul>
        <li
          v-for="profile in governance.model_profiles"
          :key="profile.id"
        >
          {{ profile.profile_kind }} — {{ profile.model_name }}
        </li>
      </ul>
    </div>
    <p v-else>{{ infoMessage ?? 'Provider governance is not available yet.' }}</p>
  </section>
</template>
