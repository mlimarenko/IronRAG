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

const sectionTabs = computed<AdminSectionTab[]>(() =>
  [
    canManageAccess.value
      ? {
          id: 'access' as AdminSectionId,
          label: t('admin.sections.access.title'),
          count: tokens.value.length,
        }
      : null,
    canManageAccess.value
      ? {
          id: 'mcp' as AdminSectionId,
          label: t('admin.sections.mcp.title'),
          count: null,
        }
      : null,
    hasOperationsSurface.value
      ? {
          id: 'operations' as AdminSectionId,
          label: t('admin.sections.operations.title'),
          count: opsSignalCount.value,
        }
      : null,
    canManageAi.value && aiConsole.value
      ? {
          id: 'ai' as AdminSectionId,
          label: t('admin.sections.ai.title'),
          count: aiSetupCount.value,
        }
      : null,
    canManageAi.value && aiConsole.value
      ? {
          id: 'pricing' as AdminSectionId,
          label: t('admin.sections.pricing.title'),
          count: pricingCount.value,
        }
      : null,
  ].filter((item): item is AdminSectionTab => item !== null),
)

watch(
  [sectionTabs, requestedSection],
  ([tabs, requested]) => {
    if (requested && tabs.some((tab) => tab.id === requested) && activeSection.value !== requested) {
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
  router.replace({ query: { ...route.query, section: val } })
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
    // clipboard access denied or unavailable — token remains visible in dialog
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
  <div class="rr-admin-control">
    <ErrorStateCard
      v-if="error && !principal"
      :title="$t('admin.title')"
      :description="error"
    />

      <template v-else-if="context && principal">
        <SurfacePanel
          v-if="loading"
          class="rr-admin-control__notice"
        >
          {{ $t('admin.loading') }}
        </SurfacePanel>

        <SurfacePanel
          v-if="error"
          class="rr-admin-control__notice rr-admin-control__notice--error"
        >
          {{ error }}
        </SurfacePanel>

        <div
          v-if="sectionTabs.length"
          class="rr-admin-control__layout"
        >
          <aside class="rr-admin-control__nav">
            <nav class="rr-admin-control__nav-list">
              <button
                v-for="tab in sectionTabs"
                :key="tab.id"
                type="button"
                class="rr-admin-control__nav-button"
                :class="{ 'is-active': activeSection === tab.id }"
                @click="activeSection = tab.id"
              >
                <span class="rr-admin-control__nav-label">{{ tab.label }}</span>
                <span
                    v-if="typeof tab.count === 'number' && tab.count > 1"
                    class="rr-admin-control__nav-count"
                  >
                  {{ tab.count }}
                </span>
              </button>
            </nav>
          </aside>

          <section class="rr-admin-control__content">
            <section
              v-if="activeSection === 'access' && canManageAccess"
              class="rr-admin-pane"
            >
              <div class="rr-admin-section__head">
                <div class="rr-admin-section__copy">
                  <p class="rr-admin-section__eyebrow">{{ $t('admin.sections.access.eyebrow') }}</p>
                  <h2>{{ $t('admin.sections.access.title') }}</h2>
                  <p>{{ $t('admin.sections.access.subtitle') }}</p>
                </div>
              </div>

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

            <section
              v-else-if="activeSection === 'mcp' && canManageAccess"
              class="rr-admin-pane"
            >
              <div class="rr-admin-section__head">
                <div class="rr-admin-section__copy">
                  <p class="rr-admin-section__eyebrow">{{ $t('admin.sections.mcp.eyebrow') }}</p>
                  <h2>{{ $t('admin.sections.mcp.title') }}</h2>
                  <p>{{ $t('admin.sections.mcp.subtitle') }}</p>
                </div>
              </div>

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
              <div class="rr-admin-section__head">
                <div class="rr-admin-section__copy">
                  <p class="rr-admin-section__eyebrow">{{ $t('admin.sections.operations.eyebrow') }}</p>
                  <h2>{{ $t('admin.sections.operations.title') }}</h2>
                  <p>{{ $t('admin.sections.operations.subtitle') }}</p>
                </div>
              </div>

              <SurfacePanel v-if="canReadOperations || canReadAudit">
                <AdminOperationsPanel
                  :snapshot="opsSnapshot"
                  :events="auditEvents"
                />
              </SurfacePanel>
            </section>

            <section
              v-else-if="activeSection === 'ai' && canManageAi && aiConsole"
              class="rr-admin-pane rr-admin-pane--editor"
            >
              <div class="rr-admin-section__head">
                <div class="rr-admin-section__copy">
                  <p class="rr-admin-section__eyebrow">{{ $t('admin.sections.ai.eyebrow') }}</p>
                  <h2>{{ $t('admin.sections.ai.title') }}</h2>
                  <p>{{ $t('admin.sections.ai.subtitle') }}</p>
                </div>
              </div>

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
              <div class="rr-admin-section__head">
                <div class="rr-admin-section__copy">
                  <p class="rr-admin-section__eyebrow">{{ $t('admin.sections.pricing.eyebrow') }}</p>
                  <h2>{{ $t('admin.sections.pricing.title') }}</h2>
                  <p>{{ $t('admin.sections.pricing.subtitle') }}</p>
                </div>
              </div>

              <AdminModelPricingPanel
                :settings="aiConsole"
                :saving="pricesSaving"
                :commit-version="catalogCommitVersion"
                :workspace-name="context.workspaceName"
                :library-name="context.libraryName"
                :error-message="pricesError"
                @create-price="createPrice"
                @update-price="updatePrice"
              />
            </section>

            <SurfacePanel
              v-else
              class="rr-admin-control__notice"
            >
              {{ $t('admin.noVisibleSections') }}
            </SurfacePanel>
          </section>
        </div>

        <SurfacePanel
          v-else
          class="rr-admin-control__notice"
        >
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
  width: min(100%, 1940px);
  min-height: calc(100vh - 8.4rem);
  margin: 0 auto;
  display: grid;
  gap: 1rem;
  align-content: start;
}

.rr-admin-control__layout {
  display: grid;
  grid-template-columns: minmax(192px, 224px) minmax(0, 1fr);
  gap: 0.85rem;
  min-height: calc(100vh - 13.5rem);
  align-items: stretch;
}

.rr-admin-control__nav {
  position: sticky;
  top: 5.6rem;
  display: grid;
  gap: 0.56rem;
  align-self: start;
  align-content: start;
  height: fit-content;
}

.rr-admin-control__content,
.rr-admin-pane {
  display: grid;
  gap: 0.9rem;
  min-height: 0;
}

.rr-admin-pane {
  min-height: 100%;
}

.rr-admin-section__head {
  display: flex;
  justify-content: space-between;
  gap: 1rem;
  align-items: end;
}

.rr-admin-section__copy {
  display: grid;
  gap: 0.08rem;
  min-width: 0;
  max-width: 54ch;
}

.rr-admin-section__eyebrow {
  margin: 0;
  font-size: 0.67rem;
  font-weight: 700;
  letter-spacing: 0.08em;
  text-transform: uppercase;
  color: var(--rr-text-muted);
}

.rr-admin-section__copy h2 {
  margin: 0.08rem 0 0.18rem;
  font-size: clamp(1.18rem, 1.45vw, 1.42rem);
  line-height: 1.1;
}

.rr-admin-section__copy p {
  margin: 0;
  color: var(--rr-text-secondary);
  font-size: 0.82rem;
  line-height: 1.36;
}

.rr-admin-control__nav-list {
  display: grid;
  gap: 0.5rem;
}

.rr-admin-control__nav-button {
  width: 100%;
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 0.55rem;
  border: 1px solid var(--rr-border-soft);
  border-radius: 13px;
  padding: 0.42rem 0.56rem;
  background: rgba(255, 255, 255, 0.78);
  color: var(--rr-text-secondary);
  font-size: 0.8rem;
  cursor: pointer;
  transition:
    background 140ms ease,
    border-color 140ms ease,
    box-shadow 140ms ease;
}

.rr-admin-control__nav-button:hover:not(.is-active) {
  background: rgba(99, 102, 241, 0.06);
  border-color: rgba(99, 102, 241, 0.12);
}

.rr-admin-control__nav-button.is-active {
  border-color: rgba(56, 87, 255, 0.25);
  color: var(--rr-text-primary);
  background: rgba(244, 247, 255, 0.96);
  box-shadow: 0 2px 8px rgba(56, 87, 255, 0.06);
}

.rr-admin-control__nav-label {
  min-width: 0;
  color: var(--rr-text-secondary);
  font-size: 0.74rem;
  font-weight: 640;
  line-height: 1.26;
}

.rr-admin-control__nav-button.is-active .rr-admin-control__nav-label {
  color: var(--rr-text-primary);
}

.rr-admin-control__nav-count {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  min-width: 0.9rem;
  height: 0.9rem;
  padding: 0 0.18rem;
  border-radius: 999px;
  border: 1px solid rgba(203, 213, 225, 0.78);
  background: rgba(248, 250, 252, 0.94);
  color: #94a3b8;
  font-size: 0.52rem;
  font-weight: 700;
  line-height: 1;
  font-variant-numeric: tabular-nums;
}

.rr-admin-control__nav-button.is-active .rr-admin-control__nav-count {
  border-color: rgba(56, 87, 255, 0.18);
  background: rgba(244, 247, 255, 0.94);
  color: #4360ea;
}

@media (min-width: 1800px) {
  .rr-admin-control {
    width: min(100%, 2120px);
    min-height: calc(100vh - 7.8rem);
    gap: 1.25rem;
  }

  .rr-admin-control__layout {
    grid-template-columns: minmax(230px, 292px) minmax(0, 1fr);
    min-height: calc(100vh - 12.8rem);
    gap: 1.25rem;
  }
}

@media (max-width: 1080px) {
  .rr-admin-control {
    gap: 0.9rem;
  }

  .rr-admin-control__layout {
    grid-template-columns: 1fr;
    gap: 0.9rem;
  }

  .rr-admin-control__nav {
    position: static;
    height: auto;
  }

  .rr-admin-control__nav-list {
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 0.5rem;
  }

  .rr-admin-control__nav-button {
    min-width: 0;
    white-space: normal;
    align-items: flex-start;
  }

  .rr-admin-section__copy p {
    font-size: 0.82rem;
    line-clamp: 2;
    -webkit-line-clamp: 2;
    display: -webkit-box;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }
}

@media (max-width: 720px) {
  .rr-admin-section__eyebrow {
    display: none;
  }

  .rr-admin-section__copy {
    gap: 0.2rem;
  }

  .rr-admin-section__copy h2 {
    margin: 0;
    font-size: 1.18rem;
  }

  .rr-admin-section__copy p {
    font-size: 0.79rem;
  }

  .rr-admin-control__nav-button {
    padding: 0.46rem 0.54rem;
    border-radius: 12px;
  }

  .rr-admin-control__nav-label {
    font-size: 0.72rem;
  }

  .rr-admin-control__nav-list {
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 0.42rem;
  }
}

@media (max-width: 600px) {
  .rr-admin-section__copy {
    gap: 0.08rem;
  }

  .rr-admin-section__copy h2 {
    font-size: 1.08rem;
  }

  .rr-admin-section__copy p {
    display: none;
  }
}

@media (max-width: 640px) {
  .rr-admin-section__head {
    flex-direction: column;
    align-items: stretch;
  }
}

@media (max-width: 420px) {
  .rr-admin-control__nav-list {
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 0.38rem;
  }

  .rr-admin-control__nav-button {
    padding: 0.42rem 0.5rem;
  }

  .rr-admin-control__nav-label {
    font-size: 0.7rem;
    line-height: 1.18;
  }
}

@media (max-width: 360px) {
  .rr-admin-control__nav-list {
    grid-template-columns: 1fr;
  }
}
</style>
