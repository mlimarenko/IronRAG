<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'

import { api, fetchProviderGovernance, type ProviderGovernanceSummary } from 'src/boot/api'
import PageSection from 'src/components/shell/PageSection.vue'
import StatusBadge from 'src/components/shell/StatusBadge.vue'
import EmptyStateCard from 'src/components/state/EmptyStateCard.vue'
import ErrorStateCard from 'src/components/state/ErrorStateCard.vue'
import LoadingSkeletonPanel from 'src/components/state/LoadingSkeletonPanel.vue'
import AppPanel from 'src/components/ui/AppPanel.vue'
import StatusBanner from 'src/components/ui/StatusBanner.vue'

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
  return (
    normalized.includes('401') ||
    normalized.includes('unauthorized') ||
    normalized.includes('authorization')
  )
}

onMounted(async () => {
  try {
    const { data } = await api.get<{ id: string }[]>('/workspaces')
    workspaceId.value = data[0]?.id ?? null

    if (!workspaceId.value) {
      infoMessage.value =
        'No workspace yet. Create a workspace before configuring provider accounts.'
      return
    }

    try {
      governance.value = await fetchProviderGovernance(workspaceId.value)
    } catch (error) {
      const message = extractErrorMessage(error)
      if (isUnauthorizedMessage(message)) {
        infoMessage.value =
          'Provider governance requires an authorized API token. Workspace discovery is working.'
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

const pageStatus = computed(() => {
  if (errorMessage.value) {
    return { status: 'blocked', label: 'Governance unavailable' }
  }

  if (loading.value) {
    return { status: 'pending', label: 'Loading provider surfaces' }
  }

  if (!workspaceId.value) {
    return { status: 'draft', label: 'Workspace required' }
  }

  if ((governance.value?.provider_accounts.length ?? 0) === 0) {
    return { status: 'partial', label: 'Accounts missing' }
  }

  return { status: 'ready', label: 'Provider foundation loaded' }
})
</script>

<template>
  <section class="rr-page-grid">
    <PageSection
      eyebrow="Operations"
      title="Providers"
      description="Configure provider accounts and model profiles on the shared section, panel, banner, and list primitives so governance views stop reinventing their own CSS."
      :status="pageStatus.status"
      :status-label="pageStatus.label"
    >
      <ErrorStateCard
        v-if="errorMessage"
        title="Provider governance unavailable"
        :message="errorMessage"
        detail="Authorization and discovery failures should land in the same shared error state used across product pages."
      />

      <LoadingSkeletonPanel v-else-if="loading" title="Loading provider surfaces" />

      <template v-else>
        <StatusBanner v-if="governance?.warning" tone="warning" :message="governance.warning" />
        <StatusBanner v-else-if="infoMessage" tone="info" :message="infoMessage" />

        <div class="rr-grid rr-grid--two">
          <AppPanel
            eyebrow="Accounts"
            title="Provider accounts"
            description="Workspace-level provider credentials and health stay visible in one place."
            tone="accent"
            :status="(governance?.provider_accounts.length ?? 0) > 0 ? 'ready' : 'draft'"
            :status-label="
              (governance?.provider_accounts.length ?? 0) > 0
                ? `${governance?.provider_accounts.length ?? 0} configured`
                : 'None configured'
            "
          >
            <EmptyStateCard
              v-if="!governance || governance.provider_accounts.length === 0"
              title="No provider accounts yet"
              :message="
                infoMessage ??
                'Create a workspace and connect at least one provider account to unlock governance details.'
              "
            />

            <ul v-else class="rr-list provider-list">
              <li v-for="provider in governance.provider_accounts" :key="provider.id">
                <div class="provider-list__row">
                  <div class="provider-list__copy">
                    <strong>{{ provider.label }}</strong>
                    <p>{{ provider.provider_kind }}</p>
                  </div>

                  <StatusBadge :status="provider.status" />
                </div>
              </li>
            </ul>
          </AppPanel>

          <AppPanel
            eyebrow="Profiles"
            title="Model profiles"
            description="Profiles define the runnable model surface once provider accounts are in place."
            :status="(governance?.model_profiles.length ?? 0) > 0 ? 'ready' : 'draft'"
            :status-label="
              (governance?.model_profiles.length ?? 0) > 0
                ? `${governance?.model_profiles.length ?? 0} available`
                : 'No profiles'
            "
          >
            <EmptyStateCard
              v-if="!governance || governance.model_profiles.length === 0"
              title="No model profiles yet"
              message="Add model profiles after provider accounts are ready so query and ingestion flows can target a stable model surface."
            />

            <ul v-else class="rr-list provider-list">
              <li v-for="profile in governance.model_profiles" :key="profile.id">
                <div class="provider-list__row">
                  <div class="provider-list__copy">
                    <strong>{{ profile.model_name }}</strong>
                    <p>{{ profile.profile_kind }}</p>
                  </div>

                  <StatusBadge status="ready" label="Configured" />
                </div>
              </li>
            </ul>
          </AppPanel>
        </div>
      </template>
    </PageSection>
  </section>
</template>

<style scoped>
.provider-list__row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--rr-space-4);
}

.provider-list__copy {
  display: grid;
  gap: 4px;
}

.provider-list__copy strong,
.provider-list__copy p {
  margin: 0;
}

.provider-list__copy p {
  color: var(--rr-color-text-muted);
}

@media (width <= 700px) {
  .provider-list__row {
    flex-direction: column;
    align-items: flex-start;
  }
}
</style>
