<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { storeToRefs } from 'pinia'
import { useI18n } from 'vue-i18n'
import ErrorStateCard from 'src/components/base/ErrorStateCard.vue'
import PageHeader from 'src/components/design-system/PageHeader.vue'
import PageSurface from 'src/components/base/PageSurface.vue'
import SurfacePanel from 'src/components/design-system/SurfacePanel.vue'
import AdminAuditFeed from 'src/components/admin/AdminAuditFeed.vue'
import AdminModelPricingPanel from 'src/components/admin/AdminModelPricingPanel.vue'
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

type AdminSectionId = 'access' | 'operations' | 'ai' | 'pricing'

const adminStore = useAdminStore()
const shellStore = useShellStore()
const { t } = useI18n()
const {
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
const pricingCount = computed(() => aiConsole.value?.prices.length ?? 0)
const activeSection = ref<AdminSectionId>('access')

const sectionTabs = computed(() =>
  [
    canManageAccess.value
      ? {
          id: 'access' as AdminSectionId,
          label: t('admin.sections.access.title'),
          count: tokens.value.length,
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
  ].filter((item): item is { id: AdminSectionId; label: string; count: number } => item !== null),
)

watch(
  sectionTabs,
  (tabs) => {
    if (tabs.some((tab) => tab.id === activeSection.value)) {
      return
    }
    activeSection.value = tabs[0]?.id ?? 'access'
  },
  { immediate: true },
)

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
  <PageSurface mode="full">
    <div class="rr-admin-control">
      <ErrorStateCard
        v-if="error && !principal"
        :title="$t('admin.title')"
        :description="error"
      />

      <template v-else-if="context && principal">
        <PageHeader
          compact
          :eyebrow="$t('shell.admin')"
          :title="$t('admin.title')"
          :subtitle="$t('admin.subtitle')"
        />

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
            <SurfacePanel
              tone="muted"
              density="compact"
              class="rr-admin-control__context-card"
            >
              <div class="rr-admin-control__context-row">
                <span>{{ $t('shell.workspace') }}</span>
                <strong>{{ context.workspaceName }}</strong>
              </div>
              <div class="rr-admin-control__context-row">
                <span>{{ $t('shell.library') }}</span>
                <strong>{{ context.libraryName }}</strong>
              </div>
            </SurfacePanel>

            <nav class="rr-admin-control__nav-list">
              <button
                v-for="tab in sectionTabs"
                :key="tab.id"
                type="button"
                class="rr-admin-control__nav-button"
                :class="{ 'is-active': activeSection === tab.id }"
                @click="activeSection = tab.id"
              >
                <span>{{ tab.label }}</span>
                <strong>{{ tab.count }}</strong>
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

                <button
                  class="rr-button rr-button--primary"
                  type="button"
                  @click="adminStore.showCreateToken = true"
                >
                  {{ $t('admin.createToken') }}
                </button>
              </div>

              <SurfacePanel class="rr-admin-pane__surface">
                <ApiTokensTable
                  embedded
                  :rows="tokens"
                  :current-principal-id="principal.id"
                  :current-principal-label="principal.displayLabel"
                  @create="adminStore.showCreateToken = true"
                  @copy="adminStore.copyToken"
                  @revoke="adminStore.revokeToken"
                />
              </SurfacePanel>
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

              <div class="rr-admin-pane__split">
                <SurfacePanel
                  v-if="canReadOperations"
                  class="rr-admin-pane__surface"
                >
                  <AdminOperationsPanel :snapshot="opsSnapshot" />
                </SurfacePanel>

                <SurfacePanel
                  v-if="canReadAudit"
                  class="rr-admin-pane__surface"
                >
                  <AdminAuditFeed :events="auditEvents" />
                </SurfacePanel>
              </div>
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
                embedded
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
                embedded
                :settings="aiConsole"
                :saving="pricesSaving"
                :commit-version="catalogCommitVersion"
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
  </PageSurface>
</template>

<style scoped lang="scss">
.rr-admin-control {
  width: min(100%, 1600px);
  margin: 0 auto;
  display: grid;
  gap: 1rem;
}

.rr-admin-control__layout {
  display: grid;
  grid-template-columns: minmax(230px, 290px) minmax(0, 1fr);
  gap: 1rem;
  align-items: start;
}

.rr-admin-control__nav {
  position: sticky;
  top: 5.6rem;
  display: grid;
  gap: 0.7rem;
}

.rr-admin-control__content,
.rr-admin-pane {
  display: grid;
  gap: 0.9rem;
}

.rr-admin-section__head {
  display: flex;
  justify-content: space-between;
  gap: 1rem;
  align-items: end;
}

.rr-admin-section__copy h2 {
  margin: 0.2rem 0 0.4rem;
  font-size: clamp(1.25rem, 1.6vw, 1.55rem);
  line-height: 1.1;
}

.rr-admin-section__copy p {
  margin: 0;
  color: var(--rr-text-secondary);
  font-size: 0.98rem;
  line-height: 1.55;
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
  gap: 0.8rem;
  border: 1px solid var(--rr-border-soft);
  border-radius: 14px;
  padding: 0.72rem 0.82rem;
  background: rgba(255, 255, 255, 0.78);
  color: var(--rr-text-secondary);
  font-size: 0.98rem;
}

.rr-admin-control__nav-button.is-active {
  border-color: rgba(56, 87, 255, 0.25);
  color: var(--rr-text-primary);
  background: rgba(244, 247, 255, 0.96);
}

.rr-admin-control__nav-button strong {
  color: var(--rr-text-primary);
  font-size: 0.92rem;
}

.rr-admin-control__context-row span {
  font-size: 0.8rem;
  color: var(--rr-text-muted);
}

.rr-admin-control__context-row strong {
  font-size: 0.94rem;
}

@media (max-width: 1080px) {
  .rr-admin-control__layout {
    grid-template-columns: 1fr;
  }

  .rr-admin-control__nav {
    position: static;
  }
}
</style>
