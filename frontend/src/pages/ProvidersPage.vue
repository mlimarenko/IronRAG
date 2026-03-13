<script setup lang="ts">
import { onMounted, ref } from 'vue'

import { api, fetchProviderGovernance, type ProviderGovernanceSummary } from 'src/boot/api'

const workspaceId = ref<string | null>(null)
const governance = ref<ProviderGovernanceSummary | null>(null)
const errorMessage = ref<string | null>(null)

onMounted(async () => {
  try {
    const { data } = await api.get<{ id: string }[]>('/v1/workspaces')
    workspaceId.value = data[0]?.id ?? null

    if (workspaceId.value) {
      governance.value = await fetchProviderGovernance(workspaceId.value)
    }
  } catch (error) {
    errorMessage.value = error instanceof Error ? error.message : 'Unknown provider error'
  }
})
</script>

<template>
  <section>
    <h2>Providers</h2>
    <p>Configure OpenAI, DeepSeek, and compatible provider accounts plus model profiles.</p>

    <p v-if="errorMessage">{{ errorMessage }}</p>
    <p v-else-if="!governance">Loading provider governance…</p>
    <div v-else>
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
  </section>
</template>
