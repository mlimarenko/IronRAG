<script setup lang="ts">
import { computed, watch } from 'vue'
import { storeToRefs } from 'pinia'
import { useI18n } from 'vue-i18n'
import ErrorStateCard from 'src/components/base/ErrorStateCard.vue'
import PageSurface from 'src/components/base/PageSurface.vue'
import AdminModelPricingPanel from 'src/components/admin/AdminModelPricingPanel.vue'
import AdminPlaceholderPanel from 'src/components/admin/AdminPlaceholderPanel.vue'
import AdminProviderSettingsPanel from 'src/components/admin/AdminProviderSettingsPanel.vue'
import AdminTabs from 'src/components/admin/AdminTabs.vue'
import ApiTokensTable from 'src/components/admin/ApiTokensTable.vue'
import CreateTokenDialog from 'src/components/admin/CreateTokenDialog.vue'
import TokenSecurityBanner from 'src/components/admin/TokenSecurityBanner.vue'
import type {
  CreateAdminCredentialPayload,
  CreateApiTokenPayload,
} from 'src/models/ui/admin'
import { useAdminStore } from 'src/stores/admin'
import { useShellStore } from 'src/stores/shell'

const { t } = useI18n()
const adminStore = useAdminStore()
const shellStore = useShellStore()
const {
  activeTab,
  aiConsole,
  auditEvents,
  bindingValidatingId,
  context,
  credentialSaving,
  error,
  latestPlaintextToken,
  loading,
  principal,
  showCreateToken,
  tabLoading,
  tokens,
  tabAvailability,
  tabCounts,
} = storeToRefs(adminStore)

watch(
  () => {
    const shellContext = shellStore.context
    if (!shellContext) {
      return null
    }
    return {
      workspaceId: shellContext.activeWorkspace.id,
      workspaceName: shellContext.activeWorkspace.name,
      libraryId: shellContext.activeLibrary.id,
      libraryName: shellContext.activeLibrary.name,
    }
  },
  async (nextContext) => {
    if (!nextContext) {
      adminStore.clearState()
      return
    }
    await adminStore.loadForContext(nextContext)
  },
  { immediate: true, deep: true },
)

const auditRows = computed(() =>
  auditEvents.value.map((event) => [
    formatDate(event.createdAt),
    event.actionKind,
    event.resultKind,
    summarizeSubjects(event.subjects),
    event.redactedMessage ?? '—',
  ]),
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

async function createCredential(payload: CreateAdminCredentialPayload) {
  await adminStore.createCredential(payload)
}

async function validateBinding(bindingId: string) {
  await adminStore.validateBinding(bindingId)
}

function formatDate(value: string): string {
  const parsed = new Date(value)
  if (Number.isNaN(parsed.getTime())) {
    return value
  }
  return parsed.toLocaleString()
}

function summarizeSubjects(
  subjects: { subjectKind: string; subjectId: string; libraryId: string | null; workspaceId: string | null }[],
): string {
  return subjects
    .map((subject) => `${subject.subjectKind}:${subject.subjectId.slice(0, 8)}`)
    .join(', ')
}
</script>

<template>
  <PageSurface>
    <div class="rr-admin">
      <header class="rr-admin__header">
        <div>
          <h1>{{ $t('admin.title') }}</h1>
          <p v-if="context">
            {{ $t('admin.subtitle', {
              workspace: context.workspaceName,
              library: context.libraryName,
            }) }}
          </p>
        </div>
        <button
          v-if="activeTab === 'tokens' && tabAvailability.tokens"
          class="rr-button"
          type="button"
          @click="adminStore.showCreateToken = true"
        >
          {{ $t('admin.createToken') }}
        </button>
      </header>

      <ErrorStateCard
        v-if="error && !principal"
        :title="$t('admin.title')"
        :description="error"
      />

      <template v-else-if="context && principal">
        <AdminTabs
          :counts="tabCounts"
          :availability="tabAvailability"
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

        <template v-else-if="activeTab === 'tokens'">
          <ApiTokensTable
            :rows="tokens"
            @copy="adminStore.copyToken"
            @revoke="adminStore.revokeToken"
          />
          <TokenSecurityBanner
            :principal="principal"
            :workspace-name="context.workspaceName"
            :library-name="context.libraryName"
          />
        </template>

        <template v-else-if="activeTab === 'aiCatalog' && aiConsole">
          <AdminProviderSettingsPanel
            :settings="aiConsole"
            :credential-saving="credentialSaving"
            :validating-binding-id="bindingValidatingId"
            @create-credential="createCredential"
            @validate-binding="validateBinding"
          />
        </template>

        <template v-else-if="activeTab === 'pricing' && aiConsole">
          <AdminModelPricingPanel :settings="aiConsole" />
        </template>

        <AdminPlaceholderPanel
          v-else-if="activeTab === 'audit'"
          :title="$t('admin.audit.title')"
          :columns="[
            t('admin.headers.created'),
            t('admin.headers.action'),
            t('admin.headers.result'),
            t('admin.headers.subjects'),
            t('admin.headers.message'),
          ]"
          :rows="auditRows"
        />

        <ErrorStateCard
          v-else
          :title="$t('admin.title')"
          :description="$t('admin.emptyState')"
        />
      </template>
    </div>

    <CreateTokenDialog
      v-if="context"
      :open="showCreateToken"
      :plaintext-token="latestPlaintextToken"
      :workspace-id="context.workspaceId"
      :workspace-name="context.workspaceName"
      :library-id="context.libraryId"
      :library-name="context.libraryName"
      @close="closeCreateDialog"
      @submit="submitCreateToken"
      @copy="copyLatestToken"
    />
  </PageSurface>
</template>
