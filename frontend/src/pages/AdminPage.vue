<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { storeToRefs } from 'pinia'
import { useI18n } from 'vue-i18n'
import ErrorStateCard from 'src/components/base/ErrorStateCard.vue'
import SurfacePanel from 'src/components/design-system/SurfacePanel.vue'
import AdminModelPricingPanel from 'src/components/admin/AdminModelPricingPanel.vue'
import AdminMcpSetupPanel from 'src/components/admin/AdminMcpSetupPanel.vue'
import AdminOperationsPanel from 'src/components/admin/AdminOperationsPanel.vue'
import AdminProviderSettingsPanel from 'src/components/admin/AdminProviderSettingsPanel.vue'
import ApiTokensTable from 'src/components/admin/ApiTokensTable.vue'
import CreateTokenDialog from 'src/components/admin/CreateTokenDialog.vue'
import type {
  CreateAdminCredentialPayload,
  CreateAdminModelPresetPayload,
  CreateAdminPricePayload,
  CreateApiTokenPayload,
  SaveAdminLibraryBindingPayload,
  UpdateAdminCredentialPayload,
  UpdateAdminModelPresetPayload,
  UpdateAdminPricePayload,
} from 'src/models/ui/admin'
import { useAdminStore } from 'src/stores/admin'
import { useShellStore } from 'src/stores/shell'

type AdminSectionId = 'access' | 'mcp' | 'operations' | 'ai' | 'pricing'
type AdminSectionTab = { id: AdminSectionId; label: string; count?: number | null }

const adminStore = useAdminStore()
const shellStore = useShellStore()
const { t } = useI18n()
const {
  accessError,
  accessSaving,
  aiConsole,
  aiSetupError,
  aiSetupSaving,
  auditEvents,
  bindingValidatingId,
  canManageAccess,
  canManageAi,
  canReadAudit,
  canReadOperations,
  catalogCommitVersion,
  context,
  error,
  latestPlaintextToken,
  loading,
  opsSnapshot,
  principal,
  pricesError,
  pricesSaving,
  showCreateToken,
  tokens,
} = storeToRefs(adminStore)

const hasOperationsSurface = computed(() => canReadOperations.value || canReadAudit.value)
const opsSignalCount = computed(
  () =>
    (opsSnapshot.value?.warnings.length ?? 0) +
    (opsSnapshot.value?.state.failedDocumentCount ?? 0) +
    (opsSnapshot.value?.state.runningAttempts ?? 0),
)
const aiSetupCount = computed(
  () =>
    (aiConsole.value?.credentials.length ?? 0) +
    (aiConsole.value?.bindings.length ?? 0) +
    (aiConsole.value?.modelPresets.length ?? 0),
)
const route = useRoute()
const router = useRouter()
const pricingCount = computed(() => aiConsole.value?.prices.length ?? 0)
const validSections: AdminSectionId[] = ['access', 'mcp', 'operations', 'ai', 'pricing']
const requestedSection = computed(() =>
  validSections.includes(route.query.section as AdminSectionId)
    ? (route.query.section as AdminSectionId)
    : null,
)
const activeSection = ref<AdminSectionId>(requestedSection.value ?? 'access')

const sectionTabs = computed<AdminSectionTab[]>(() => {
  const tabs: AdminSectionTab[] = []

  if (canManageAccess.value) {
    tabs.push({
      id: 'access',
      label: t('admin.sections.access.title'),
      count: tokens.value.length > 0 ? tokens.value.length : null,
    })
    tabs.push({
      id: 'mcp',
      label: t('admin.sections.mcp.title'),
      count: null,
    })
  }

  if (hasOperationsSurface.value) {
    tabs.push({
      id: 'operations',
      label: t('admin.sections.operations.title'),
      count: opsSignalCount.value > 0 ? opsSignalCount.value : null,
    })
  }

  if (canManageAi.value && aiConsole.value) {
    tabs.push({
      id: 'ai',
      label: t('admin.sections.ai.title'),
      count: aiSetupCount.value > 0 ? aiSetupCount.value : null,
    })
    tabs.push({
      id: 'pricing',
      label: t('admin.sections.pricing.title'),
      count: pricingCount.value,
    })
  }

  return tabs
})

const navContextLabel = computed(() => {
  if (!context.value) {
    return ''
  }
  return `${context.value.workspaceName} · ${context.value.libraryName}`
})

watch(
  [sectionTabs, requestedSection],
  ([tabs, requested]) => {
    if (
      requested &&
      tabs.some((tab) => tab.id === requested) &&
      activeSection.value !== requested
    ) {
      activeSection.value = requested
      return
    }
    if (tabs.some((tab) => tab.id === activeSection.value)) {
      return
    }
    activeSection.value = tabs[0]?.id ?? 'access'
  },
  { immediate: true },
)

watch(activeSection, (val) => {
  void router.replace({ query: { ...route.query, section: val } })
})

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
    try {
      await adminStore.loadForContext(nextContext)
    } catch {
      // Store error state is authoritative for page feedback.
    }
  },
  { immediate: true, deep: true },
)

async function copyLatestToken() {
  if (!latestPlaintextToken.value) {
    return
  }
  try {
    await navigator.clipboard.writeText(latestPlaintextToken.value)
  } catch {
    return
  }
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

async function updateCredential(payload: UpdateAdminCredentialPayload) {
  await adminStore.updateCredential(payload)
}

async function createModelPreset(payload: CreateAdminModelPresetPayload) {
  await adminStore.createModelPreset(payload)
}

async function updateModelPreset(payload: UpdateAdminModelPresetPayload) {
  await adminStore.updateModelPreset(payload)
}

async function saveBinding(payload: SaveAdminLibraryBindingPayload) {
  await adminStore.saveBinding(payload)
}

async function createPrice(payload: CreateAdminPricePayload) {
  await adminStore.createPrice(payload)
}

async function updatePrice(payload: UpdateAdminPricePayload) {
  await adminStore.updatePrice(payload)
}

async function validateBinding(bindingId: string) {
  await adminStore.validateBinding(bindingId)
}
</script>

<template>
  <div class="rr-admin-control" :class="`is-${activeSection}`">
    <ErrorStateCard v-if="error && !principal" :title="$t('admin.title')" :description="error" />

    <template v-else-if="context && principal">
      <SurfacePanel v-if="error" class="rr-admin-control__notice rr-admin-control__notice--error">
        {{ error }}
      </SurfacePanel>

      <div v-if="sectionTabs.length" class="rr-admin-control__layout">
        <aside class="rr-admin-control__nav">
          <div class="rr-admin-control__nav-card">
            <div class="rr-admin-control__nav-copy">
              <h1>{{ $t('admin.title') }}</h1>
              <p v-if="navContextLabel">{{ navContextLabel }}</p>
            </div>

            <nav class="rr-admin-control__tabs" aria-label="Admin sections">
              <button
                v-for="tab in sectionTabs"
                :key="tab.id"
                type="button"
                class="rr-admin-control__tab"
                :class="{ 'is-active': activeSection === tab.id }"
                @click="activeSection = tab.id"
              >
                <span>{{ tab.label }}</span>
                <span v-if="typeof tab.count === 'number'" class="rr-admin-control__tab-count">
                  {{ tab.count }}
                </span>
              </button>
            </nav>
          </div>
        </aside>

        <section class="rr-admin-control__content">
          <section v-if="activeSection === 'access' && canManageAccess" class="rr-admin-pane">
            <ApiTokensTable
              :rows="tokens"
              :current-principal-id="principal.id"
              :current-principal-label="principal.displayLabel"
              :workspace-name="context.workspaceName"
              :library-name="context.libraryName"
              :loading="loading || accessSaving"
              :error-message="accessError"
              @create="adminStore.showCreateToken = true"
              @copy="adminStore.copyToken"
              @revoke="adminStore.revokeToken"
            />
          </section>

          <section v-else-if="activeSection === 'mcp' && canManageAccess" class="rr-admin-pane">
            <AdminMcpSetupPanel
              :workspace-name="context.workspaceName"
              :library-name="context.libraryName"
              @create-token="adminStore.showCreateToken = true"
            />
          </section>

          <section
            v-else-if="activeSection === 'operations' && hasOperationsSurface"
            class="rr-admin-pane"
          >
            <AdminOperationsPanel
              v-if="canReadOperations || canReadAudit"
              :snapshot="opsSnapshot"
              :events="auditEvents"
            />
          </section>

          <section
            v-else-if="activeSection === 'ai' && canManageAi && aiConsole"
            class="rr-admin-pane rr-admin-pane--editor"
          >
            <AdminProviderSettingsPanel
              :settings="aiConsole"
              :saving="aiSetupSaving"
              :validating-binding-id="bindingValidatingId"
              :commit-version="catalogCommitVersion"
              :error-message="aiSetupError"
              @create-credential="createCredential"
              @update-credential="updateCredential"
              @create-model-preset="createModelPreset"
              @update-model-preset="updateModelPreset"
              @save-binding="saveBinding"
              @validate-binding="validateBinding"
            />
          </section>

          <section
            v-else-if="activeSection === 'pricing' && canManageAi && aiConsole"
            class="rr-admin-pane rr-admin-pane--editor"
          >
            <AdminModelPricingPanel
              :settings="aiConsole"
              :saving="pricesSaving"
              :commit-version="catalogCommitVersion"
              :error-message="pricesError"
              @create-price="createPrice"
              @update-price="updatePrice"
            />
          </section>

          <SurfacePanel v-else class="rr-admin-control__notice">
            {{ $t('admin.noVisibleSections') }}
          </SurfacePanel>
        </section>
      </div>

      <SurfacePanel v-else class="rr-admin-control__notice">
        {{ $t('admin.noVisibleSections') }}
      </SurfacePanel>
    </template>
  </div>

  <CreateTokenDialog
    v-if="context && canManageAccess"
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
</template>

<style scoped lang="scss">
.rr-admin-control {
  width: min(100%, 1560px);
  min-height: calc(100vh - 8.1rem);
  margin: 0 auto;
  display: grid;
  gap: 1.1rem;
  align-content: start;
  padding: 0 10px 22px;
}

.rr-admin-control__layout,
.rr-admin-control__content,
.rr-admin-pane {
  display: grid;
  gap: 0.95rem;
  min-height: 0;
}

.rr-admin-control__layout {
  grid-template-columns: 220px minmax(0, 1fr);
  align-items: start;
}

.rr-admin-control__nav {
  position: sticky;
  top: 5rem;
  align-self: start;
}

.rr-admin-control__nav-card {
  display: grid;
  gap: 0.85rem;
  padding: 0.9rem;
  border: 1px solid rgba(226, 232, 240, 0.9);
  border-radius: 16px;
  background: rgba(255, 255, 255, 0.94);
  box-shadow: 0 6px 16px rgba(15, 23, 42, 0.025);
}

.rr-admin-control__nav-copy {
  display: grid;
  gap: 0.2rem;
}

.rr-admin-control__nav-copy h1 {
  margin: 0;
  font-size: 1rem;
  line-height: 1.15;
}

.rr-admin-control__nav-copy p {
  margin: 0;
  color: var(--rr-text-secondary);
  font-size: 0.78rem;
  line-height: 1.4;
}

.rr-admin-pane {
  min-height: 100%;
}

.rr-admin-control.is-access .rr-admin-pane,
.rr-admin-control.is-mcp .rr-admin-pane {
  width: min(100%, 1160px);
  justify-self: start;
}

.rr-admin-control.is-operations .rr-admin-pane {
  width: min(100%, 1220px);
  justify-self: start;
}

.rr-admin-control.is-ai .rr-admin-pane,
.rr-admin-control.is-pricing .rr-admin-pane {
  width: min(100%, 1320px);
  justify-self: start;
}

.rr-admin-control__tabs {
  display: grid;
  gap: 0.38rem;
  padding: 0;
}

.rr-admin-control__tab {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 0.42rem;
  width: 100%;
  min-height: 2.5rem;
  padding: 0.56rem 0.78rem;
  border: 1px solid rgba(226, 232, 240, 0.9);
  border-radius: 12px;
  background: rgba(255, 255, 255, 0.92);
  color: var(--rr-text-secondary);
  font-size: 0.78rem;
  font-weight: 600;
  cursor: pointer;
  transition:
    background 140ms ease,
    border-color 140ms ease,
    box-shadow 140ms ease;
}

.rr-admin-control__tab:hover:not(.is-active) {
  background: rgba(99, 102, 241, 0.06);
  border-color: rgba(99, 102, 241, 0.12);
}

.rr-admin-control__tab.is-active {
  color: #334155;
  background: rgba(244, 247, 255, 0.98);
  border-color: rgba(99, 102, 241, 0.24);
  box-shadow: 0 4px 12px rgba(99, 102, 241, 0.06);
}

.rr-admin-control__tab-count {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  min-width: 1.14rem;
  min-height: 1.14rem;
  padding: 0 0.32rem;
  border-radius: 999px;
  background: rgba(241, 245, 249, 0.98);
  color: var(--rr-text-muted);
  font-size: 0.64rem;
  line-height: 1;
}

@media (max-width: 900px) {
  .rr-admin-control {
    padding-inline: 10px;
  }

  .rr-admin-control__layout {
    grid-template-columns: 1fr;
  }

  .rr-admin-control__nav {
    position: static;
  }

  .rr-admin-control__nav-card {
    padding: 0;
    border: 0;
    border-radius: 0;
    background: transparent;
    box-shadow: none;
  }

  .rr-admin-control__tabs {
    display: flex;
    flex-wrap: wrap;
    gap: 0.42rem;
  }

  .rr-admin-control__tab {
    width: auto;
    min-height: 2.1rem;
    padding: 0.42rem 0.72rem;
    border-radius: 999px;
  }
}

@media (max-width: 640px) {
  .rr-admin-control__nav-copy {
    display: none;
  }

  .rr-admin-control__tab {
    min-height: 2.05rem;
    font-size: 0.76rem;
  }
}
</style>
