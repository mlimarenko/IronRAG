<script setup lang="ts">
import { computed, watch } from 'vue'
import { storeToRefs } from 'pinia'
import { useI18n } from 'vue-i18n'
import ErrorStateCard from 'src/components/base/ErrorStateCard.vue'
import PageSurface from 'src/components/base/PageSurface.vue'
import AdminPlaceholderPanel from 'src/components/admin/AdminPlaceholderPanel.vue'
import AdminModelPricingPanel from 'src/components/admin/AdminModelPricingPanel.vue'
import AdminProviderSettingsPanel from 'src/components/admin/AdminProviderSettingsPanel.vue'
import AdminTabs from 'src/components/admin/AdminTabs.vue'
import ApiTokensTable from 'src/components/admin/ApiTokensTable.vue'
import CreateTokenDialog from 'src/components/admin/CreateTokenDialog.vue'
import TokenSecurityBanner from 'src/components/admin/TokenSecurityBanner.vue'
import type {
  AdminUpsertPricingEntryPayload,
  CreateApiTokenPayload,
  UpdateAdminProviderProfilePayload,
} from 'src/models/ui/admin'
import { useAdminStore } from 'src/stores/admin'
import { useShellStore } from 'src/stores/shell'

const { t } = useI18n()
const adminStore = useAdminStore()
const shellStore = useShellStore()
const {
  activeTab,
  error,
  latestPlaintextToken,
  libraryAccess,
  loading,
  members,
  overview,
  settings,
  pricingSaving,
  settingsSaving,
  settingsValidating,
  showCreateToken,
  tabLoading,
  tokens,
} = storeToRefs(adminStore)

watch(
  () => {
    const context = shellStore.context
    return context ? `${context.activeWorkspace.id}:${context.activeLibrary.id}` : null
  },
  async (contextKey) => {
    if (!contextKey) {
      return
    }
    await adminStore.loadOverview()
  },
  { immediate: true },
)

const memberRows = computed(() =>
  members.value.map((row) => [row.displayName, row.email, row.roleLabel]),
)
const libraryAccessRows = computed(() =>
  libraryAccess.value.map((row) => [row.libraryName, row.principalLabel, row.accessLevel]),
)

async function copyLatestToken() {
  if (!latestPlaintextToken.value) {
    return
  }
  await navigator.clipboard.writeText(latestPlaintextToken.value)
  adminStore.showCreateToken = false
  adminStore.clearLatestPlaintextToken()
}

function closeCreateDialog() {
  adminStore.showCreateToken = false
  adminStore.clearLatestPlaintextToken()
}

async function submitCreateToken(payload: CreateApiTokenPayload) {
  await adminStore.createToken(payload)
}

async function saveProviderProfile(payload: UpdateAdminProviderProfilePayload) {
  await adminStore.saveProviderProfile(payload)
}

async function validateProviderProfile() {
  await adminStore.validateProviderProfile()
}

async function createPricingEntry(payload: AdminUpsertPricingEntryPayload) {
  await adminStore.createPricingEntry(payload)
}

async function updatePricingEntry(
  pricingId: string,
  payload: AdminUpsertPricingEntryPayload,
) {
  await adminStore.updatePricingEntry(pricingId, payload)
}

async function deactivatePricingEntry(pricingId: string) {
  await adminStore.deactivatePricingEntry(pricingId)
}
</script>

<template>
  <PageSurface>
    <div class="rr-admin">
      <header class="rr-admin__header">
        <div>
          <h1>{{ $t('admin.title') }}</h1>
          <p>{{ overview?.workspaceName ?? $t('admin.subtitle') }}</p>
        </div>
        <button
          v-if="activeTab === 'api_tokens'"
          class="rr-button"
          type="button"
          @click="adminStore.showCreateToken = true"
        >
          {{ $t('admin.createToken') }}
        </button>
      </header>

      <ErrorStateCard
        v-if="error && !overview"
        :title="$t('admin.title')"
        :description="error"
      />

      <template v-else-if="overview">
        <AdminTabs
          :overview="overview"
          :active-tab="activeTab"
          @change="adminStore.switchTab"
        />

        <ErrorStateCard
          v-if="error"
          :title="$t('admin.title')"
          :description="error"
        />

        <p
          v-if="loading || tabLoading"
          class="rr-admin__loading"
        >
          {{ $t('admin.loading') }}
        </p>

        <template v-else-if="activeTab === 'api_tokens'">
          <ApiTokensTable
            :rows="tokens"
            @copy="adminStore.copyToken"
            @revoke="adminStore.revokeToken"
          />
          <TokenSecurityBanner />
        </template>

        <AdminPlaceholderPanel
          v-else-if="activeTab === 'members'"
          :title="$t('admin.tabs.members')"
          :columns="[
            t('admin.headers.member'),
            t('admin.headers.email'),
            t('admin.headers.role'),
          ]"
          :rows="memberRows"
        />

        <AdminPlaceholderPanel
          v-else-if="activeTab === 'library_access'"
          :title="$t('admin.tabs.libraryAccess')"
          :columns="[
            t('admin.headers.library'),
            t('admin.headers.principal'),
            t('admin.headers.access'),
          ]"
          :rows="libraryAccessRows"
        />

        <template v-else-if="settings">
          <AdminProviderSettingsPanel
            :settings="settings"
            :saving="settingsSaving"
            :validating="settingsValidating"
            @save="saveProviderProfile"
            @validate="validateProviderProfile"
          />
          <AdminModelPricingPanel
            :settings="settings"
            :saving="pricingSaving"
            @create="createPricingEntry"
            @update="updatePricingEntry"
            @deactivate="deactivatePricingEntry"
          />
        </template>

        <ErrorStateCard
          v-else
          :title="$t('admin.tabs.settings')"
          :description="$t('admin.settings.emptyState')"
        />
      </template>
    </div>

    <CreateTokenDialog
      :open="showCreateToken"
      :plaintext-token="latestPlaintextToken"
      @close="closeCreateDialog"
      @submit="submitCreateToken"
      @copy="copyLatestToken"
    />
  </PageSurface>
</template>
