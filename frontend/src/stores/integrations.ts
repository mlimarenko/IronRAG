import { computed, ref } from 'vue'
import { defineStore } from 'pinia'

import { backendUrl } from 'src/boot/api'

export interface IntegrationEndpoint {
  key: string
  label: string
  baseUrl: string
  status: 'configured' | 'unknown'
}

export const useIntegrationsStore = defineStore('integrations', () => {
  const endpoints = ref<IntegrationEndpoint[]>([
    {
      key: 'backend',
      label: 'RustRAG API',
      baseUrl: backendUrl,
      status: 'configured',
    },
  ])

  const hasConfiguredEndpoints = computed(() =>
    endpoints.value.some((endpoint) => endpoint.status === 'configured'),
  )

  return {
    endpoints,
    hasConfiguredEndpoints,
  }
})
