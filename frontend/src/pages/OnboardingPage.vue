<script setup lang="ts">
import { computed, onMounted, reactive, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { useRouter } from 'vue-router'

import type { CreateModelProfileRequest, CreateProviderAccountRequest, CreateProjectRequest, CreateSourceRequest, CreateWorkspaceRequest } from 'src/boot/api'
import PageSection from 'src/components/shell/PageSection.vue'
import StatusBadge from 'src/components/shell/StatusBadge.vue'
import EmptyStateCard from 'src/components/state/EmptyStateCard.vue'
import ErrorStateCard from 'src/components/state/ErrorStateCard.vue'
import LoadingSkeletonPanel from 'src/components/state/LoadingSkeletonPanel.vue'
import { useDocumentsStore } from 'src/stores/documents'
import { useProjectsStore } from 'src/stores/projects'
import { useProvidersStore } from 'src/stores/providers'
import { useWorkspacesStore } from 'src/stores/workspaces'

const { t } = useI18n()
const router = useRouter()

const workspacesStore = useWorkspacesStore()
const projectsStore = useProjectsStore()
const providersStore = useProvidersStore()
const documentsStore = useDocumentsStore()

const loading = ref(true)
const refreshError = ref<string | null>(null)
const flashMessage = ref<string | null>(null)
const latestResult = ref<string | null>(null)
const selectedWorkspaceId = ref<string | null>(null)
const selectedProjectId = ref<string | null>(null)

const workspaceForm = reactive<CreateWorkspaceRequest>({
  name: 'Default workspace',
  slug: 'default-workspace',
})

const projectForm = reactive({
  workspace_id: '',
  name: 'Knowledge base',
  slug: 'knowledge-base',
  description: 'First operator-facing RAG project.',
})

const providerForm = reactive({
  workspace_id: '',
  provider_kind: 'openai',
  label: 'Primary OpenAI account',
  api_base_url: '',
  profile_kind: 'chat',
  model_name: 'gpt-4.1-mini',
  temperature: null as number | null,
  max_output_tokens: null as number | null,
})

const documentForm = reactive({
  project_id: '',
  source_kind: 'upload',
  source_label: 'Seed document',
  external_key: 'seed-doc-001',
  title: 'Welcome to RustRAG',
  text: 'RustRAG onboarding document. Replace this with your first real corpus entry.',
  queueJob: true,
})

const workspaces = computed(() => workspacesStore.items)
const projects = computed(() => projectsStore.items)
const currentWorkspace = computed(() => {
  if (selectedWorkspaceId.value) {
    return workspaces.value.find((item) => item.id === selectedWorkspaceId.value) ?? null
  }

  return workspaces.value[0] ?? null
})
const currentProject = computed(() => {
  if (selectedProjectId.value) {
    return projects.value.find((item) => item.id === selectedProjectId.value) ?? null
  }

  return projects.value[0] ?? null
})
const providerState = computed(() => {
  const workspaceId = selectedWorkspaceId.value
  if (!workspaceId) {
    return null
  }

  return providersStore.governanceByWorkspaceId[workspaceId].data ?? null
})
const projectDocumentState = computed(() => {
  const projectId = selectedProjectId.value
  if (!projectId) {
    return null
  }

  return documentsStore.byProjectId[projectId] ?? null
})
const readiness = computed(() => {
  const projectId = selectedProjectId.value
  if (!projectId) {
    return null
  }

  return projectsStore.readinessById[projectId].data ?? null
})

const checklist = computed(() => {
  const governance = providerState.value
  const documents = projectDocumentState.value?.documents.data ?? []

  return [
    {
      key: 'workspace',
      title: t('onboarding.steps.workspace.title'),
      summary: t('onboarding.steps.workspace.summary'),
      complete: workspaces.value.length > 0,
    },
    {
      key: 'project',
      title: t('onboarding.steps.project.title'),
      summary: t('onboarding.steps.project.summary'),
      complete: projects.value.length > 0,
    },
    {
      key: 'provider',
      title: t('onboarding.steps.provider.title'),
      summary: t('onboarding.steps.provider.summary'),
      complete:
        (governance?.provider_accounts.length ?? 0) > 0 &&
        (governance?.model_profiles.length ?? 0) > 0,
    },
    {
      key: 'document',
      title: t('onboarding.steps.document.title'),
      summary: t('onboarding.steps.document.summary'),
      complete: documents.length > 0,
    },
  ]
})

const completedSteps = computed(() => checklist.value.filter((step) => step.complete).length)
const progressPercent = computed(() => Math.round((completedSteps.value / checklist.value.length) * 100))
const surfaceStatus = computed(() => {
  if (refreshError.value) {
    return 'Failed'
  }
  if (completedSteps.value === checklist.value.length) {
    return 'Healthy'
  }
  if (completedSteps.value > 0) {
    return 'Warning'
  }
  return 'Pending'
})

function extractError(error: unknown): string {
  return error instanceof Error ? error.message : 'Unknown onboarding error'
}

function syncSelectionDefaults() {
  selectedWorkspaceId.value ??= workspaces.value[0]?.id ?? null

  if (selectedWorkspaceId.value) {
    projectForm.workspace_id = selectedWorkspaceId.value
    providerForm.workspace_id = selectedWorkspaceId.value
  }

  selectedProjectId.value ??=
    projects.value.find((item) => item.workspace_id === selectedWorkspaceId.value)?.id ?? null

  if (selectedProjectId.value) {
    documentForm.project_id = selectedProjectId.value
  }
}

async function refreshState() {
  refreshError.value = null

  try {
    await workspacesStore.fetchList()
    syncSelectionDefaults()

    if (selectedWorkspaceId.value) {
      await Promise.all([
        workspacesStore.fetchGovernance(selectedWorkspaceId.value).catch(() => null),
        projectsStore.fetchList(selectedWorkspaceId.value),
        providersStore.fetchGovernance(selectedWorkspaceId.value).catch(() => null),
        providersStore.fetchAccounts(selectedWorkspaceId.value).catch(() => null),
        providersStore.fetchModelProfilesForWorkspace(selectedWorkspaceId.value).catch(() => null),
      ])
    } else {
      projectsStore.listState.data = []
    }

    syncSelectionDefaults()

    if (selectedProjectId.value) {
      await Promise.all([
        projectsStore.fetchReadiness(selectedProjectId.value).catch(() => null),
        documentsStore.fetchProjectDocuments(selectedProjectId.value).catch(() => null),
        documentsStore.fetchProjectJobs(selectedProjectId.value).catch(() => null),
      ])
    }
  } catch (error) {
    refreshError.value = extractError(error)
  }
}

async function submitWorkspace() {
  try {
    const created = await workspacesStore.createItem({
      name: workspaceForm.name.trim(),
      slug: workspaceForm.slug.trim(),
    })
    selectedWorkspaceId.value = created.id
    selectedProjectId.value = null
    flashMessage.value = t('onboarding.messages.workspaceCreated')
    latestResult.value = `${created.name} (${created.slug})`
    await refreshState()
  } catch (error) {
    refreshError.value = extractError(error)
  }
}

async function submitProject() {
  if (!selectedWorkspaceId.value) {
    return
  }

  try {
    const payload: CreateProjectRequest = {
      workspace_id: selectedWorkspaceId.value,
      name: projectForm.name.trim(),
      slug: projectForm.slug.trim(),
      description: projectForm.description.trim() || null,
    }
    const created = await projectsStore.createItem(payload)
    selectedProjectId.value = created.id
    flashMessage.value = t('onboarding.messages.projectCreated')
    latestResult.value = `${created.name} (${created.slug})`
    await refreshState()
  } catch (error) {
    refreshError.value = extractError(error)
  }
}

async function submitProvider() {
  if (!selectedWorkspaceId.value) {
    return
  }

  try {
    const accountPayload: CreateProviderAccountRequest = {
      workspace_id: selectedWorkspaceId.value,
      provider_kind: providerForm.provider_kind.trim(),
      label: providerForm.label.trim(),
      api_base_url: providerForm.api_base_url.trim() || null,
    }
    const account = await providersStore.createAccount(accountPayload)

    const profilePayload: CreateModelProfileRequest = {
      workspace_id: selectedWorkspaceId.value,
      provider_account_id: account.id,
      profile_kind: providerForm.profile_kind.trim(),
      model_name: providerForm.model_name.trim(),
      temperature: providerForm.temperature,
      max_output_tokens: providerForm.max_output_tokens,
    }
    await providersStore.createProfile(profilePayload)

    flashMessage.value = t('onboarding.messages.providerCreated')
    latestResult.value = `${account.label} → ${providerForm.model_name}`
    await refreshState()
  } catch (error) {
    refreshError.value = extractError(error)
  }
}

async function submitDocument() {
  if (!selectedProjectId.value) {
    return
  }

  try {
    const sourcePayload: CreateSourceRequest = {
      project_id: selectedProjectId.value,
      source_kind: documentForm.source_kind.trim(),
      label: documentForm.source_label.trim(),
    }
    const source = await documentsStore.createSourceForProject(sourcePayload)

    const ingested = await documentsStore.ingestTextForProject({
      project_id: selectedProjectId.value,
      source_id: source.id,
      external_key: documentForm.external_key.trim(),
      title: documentForm.title.trim() || null,
      text: documentForm.text,
    })

    if (documentForm.queueJob) {
      await documentsStore.createJobForProject({
        project_id: selectedProjectId.value,
        source_id: source.id,
        trigger_kind: 'manual',
        requested_by: 'onboarding-ui',
      })
    }

    flashMessage.value = t('onboarding.messages.documentCreated')
    latestResult.value = `${ingested.documentId} · ${String(ingested.chunkCount)} chunks`
    await refreshState()
  } catch (error) {
    refreshError.value = extractError(error)
  }
}

onMounted(async () => {
  await refreshState()
  loading.value = false
})
</script>

<template>
  <PageSection
    :eyebrow="t('onboarding.eyebrow')"
    :title="t('onboarding.title')"
    :description="t('onboarding.description')"
    :status="surfaceStatus"
  >
    <template #actions>
      <q-btn
        color="primary"
        outline
        icon="refresh"
        :label="t('onboarding.actions.refresh')"
        @click="refreshState"
      />
      <q-btn
        flat
        icon="folder"
        :label="t('onboarding.actions.openProjects')"
        @click="router.push('/projects')"
      />
      <q-btn
        flat
        icon="hub"
        :label="t('onboarding.actions.openProviders')"
        @click="router.push('/providers')"
      />
      <q-btn
        flat
        icon="upload"
        :label="t('onboarding.actions.openIngestion')"
        @click="router.push('/ingestion')"
      />
    </template>

    <LoadingSkeletonPanel
      v-if="loading"
      :title="t('common.loading')"
      :lines="7"
    />

    <template v-else>
      <ErrorStateCard
        v-if="refreshError"
        :title="t('errors.requestFailed')"
        :message="refreshError"
      />

      <article class="onboarding-hero">
        <div class="onboarding-hero__copy">
          <p class="onboarding-hero__eyebrow">{{ t('onboarding.progressLabel') }}</p>
          <h2>{{ completedSteps }}/{{ checklist.length }}</h2>
          <p>{{ t('onboarding.setupChecklistHint') }}</p>
          <div
            v-if="flashMessage || latestResult"
            class="result-chip"
          >
            <strong>{{ flashMessage }}</strong>
            <span v-if="latestResult">{{ latestResult }}</span>
          </div>
        </div>
        <div class="onboarding-hero__progress">
          <q-circular-progress
            show-value
            font-size="18px"
            :value="progressPercent"
            size="120px"
            :thickness="0.18"
            color="primary"
            track-color="blue-1"
            class="glossy"
          >
            {{ progressPercent }}%
          </q-circular-progress>
        </div>
      </article>

      <div class="onboarding-grid">
        <article class="panel checklist-panel">
          <div class="panel__header">
            <div>
              <h3>{{ t('onboarding.setupChecklistTitle') }}</h3>
              <p>{{ t('onboarding.setupChecklistHint') }}</p>
            </div>
            <StatusBadge
              :status="surfaceStatus"
              :label="`${completedSteps}/${checklist.length}`"
            />
          </div>

          <ol class="checklist">
            <li
              v-for="step in checklist"
              :key="step.key"
              :data-complete="step.complete"
            >
              <div class="checklist__icon">
                <q-icon
                  :name="step.complete ? 'check_circle' : 'radio_button_unchecked'"
                  size="20px"
                />
              </div>
              <div>
                <strong>{{ step.title }}</strong>
                <p>{{ step.summary }}</p>
              </div>
              <StatusBadge
                :status="step.complete ? 'Healthy' : 'Pending'"
                :label="
                  step.complete
                    ? t('onboarding.statuses.complete')
                    : t('onboarding.statuses.pending')
                "
              />
            </li>
          </ol>
        </article>

        <article class="panel metrics-panel">
          <h3>{{ t('onboarding.cards.currentState') }}</h3>
          <div class="metric-grid">
            <div>
              <span>{{ t('onboarding.metrics.workspaces') }}</span>
              <strong>{{ workspaces.length }}</strong>
            </div>
            <div>
              <span>{{ t('onboarding.metrics.projects') }}</span>
              <strong>{{ projects.length }}</strong>
            </div>
            <div>
              <span>{{ t('onboarding.metrics.providerAccounts') }}</span>
              <strong>{{ providerState?.provider_accounts.length ?? 0 }}</strong>
            </div>
            <div>
              <span>{{ t('onboarding.metrics.modelProfiles') }}</span>
              <strong>{{ providerState?.model_profiles.length ?? 0 }}</strong>
            </div>
            <div>
              <span>{{ t('onboarding.metrics.documents') }}</span>
              <strong>{{ projectDocumentState?.documents.data.length ?? 0 }}</strong>
            </div>
            <div>
              <span>{{ t('onboarding.metrics.jobs') }}</span>
              <strong>{{ projectDocumentState?.jobs.data.length ?? 0 }}</strong>
            </div>
          </div>
        </article>
      </div>

      <div class="forms-grid">
        <article class="panel form-panel">
          <div class="panel__header">
            <div>
              <h3>{{ t('onboarding.steps.workspace.title') }}</h3>
              <p>{{ t('onboarding.steps.workspace.summary') }}</p>
            </div>
            <StatusBadge
              :status="workspaces.length > 0 ? 'Healthy' : 'Pending'"
              :label="
                workspaces.length > 0
                  ? t('onboarding.steps.workspace.complete')
                  : t('onboarding.statuses.pending')
              "
            />
          </div>
          <q-form
            class="form-stack"
            @submit.prevent="submitWorkspace"
          >
            <q-input
              v-model="workspaceForm.name"
              outlined
              :label="t('onboarding.fields.workspaceName')"
            />
            <q-input
              v-model="workspaceForm.slug"
              outlined
              :label="t('onboarding.fields.workspaceSlug')"
              :hint="t('onboarding.hints.slug')"
            />
            <q-btn
              type="submit"
              color="primary"
              :label="t('onboarding.steps.workspace.action')"
              :loading="workspacesStore.createState.status === 'loading'"
            />
          </q-form>
        </article>

        <article class="panel form-panel">
          <div class="panel__header">
            <div>
              <h3>{{ t('onboarding.steps.project.title') }}</h3>
              <p>{{ t('onboarding.steps.project.summary') }}</p>
            </div>
            <StatusBadge
              :status="projects.length > 0 ? 'Healthy' : selectedWorkspaceId ? 'Pending' : 'Blocked'"
              :label="
                projects.length > 0
                  ? t('onboarding.steps.project.complete')
                  : selectedWorkspaceId
                    ? t('onboarding.statuses.pending')
                    : t('onboarding.statuses.blocked')
              "
            />
          </div>
          <q-form
            class="form-stack"
            @submit.prevent="submitProject"
          >
            <q-select
              v-model="selectedWorkspaceId"
              outlined
              emit-value
              map-options
              option-value="id"
              option-label="name"
              :label="t('common.workspace')"
              :options="workspaces"
              @update:model-value="syncSelectionDefaults"
            />
            <q-input
              v-model="projectForm.name"
              outlined
              :label="t('onboarding.fields.projectName')"
              :disable="!selectedWorkspaceId"
            />
            <q-input
              v-model="projectForm.slug"
              outlined
              :label="t('onboarding.fields.projectSlug')"
              :disable="!selectedWorkspaceId"
            />
            <q-input
              v-model="projectForm.description"
              outlined
              type="textarea"
              autogrow
              :label="t('onboarding.fields.projectDescription')"
              :disable="!selectedWorkspaceId"
            />
            <q-btn
              type="submit"
              color="primary"
              :disable="!selectedWorkspaceId"
              :label="t('onboarding.steps.project.action')"
              :loading="projectsStore.createState.status === 'loading'"
            />
          </q-form>
          <EmptyStateCard
            v-if="!selectedWorkspaceId"
            :title="t('onboarding.statuses.blocked')"
            :message="t('onboarding.empty.noWorkspace')"
          />
        </article>

        <article class="panel form-panel">
          <div class="panel__header">
            <div>
              <h3>{{ t('onboarding.steps.provider.title') }}</h3>
              <p>{{ t('onboarding.steps.provider.summary') }}</p>
            </div>
            <StatusBadge
              :status="
                providerState &&
                  providerState.provider_accounts.length > 0 &&
                  providerState.model_profiles.length > 0
                  ? 'Healthy'
                  : selectedWorkspaceId
                    ? 'Pending'
                    : 'Blocked'
              "
              :label="
                providerState &&
                  providerState.provider_accounts.length > 0 &&
                  providerState.model_profiles.length > 0
                  ? t('onboarding.steps.provider.complete')
                  : selectedWorkspaceId
                    ? t('onboarding.statuses.pending')
                    : t('onboarding.statuses.blocked')
              "
            />
          </div>
          <q-form
            class="form-stack"
            @submit.prevent="submitProvider"
          >
            <q-select
              v-model="selectedWorkspaceId"
              outlined
              emit-value
              map-options
              option-value="id"
              option-label="name"
              :label="t('common.workspace')"
              :options="workspaces"
              @update:model-value="syncSelectionDefaults"
            />
            <q-input
              v-model="providerForm.label"
              outlined
              :label="t('onboarding.fields.providerLabel')"
              :disable="!selectedWorkspaceId"
            />
            <q-select
              v-model="providerForm.provider_kind"
              outlined
              :label="t('onboarding.fields.providerKind')"
              :options="['openai', 'deepseek', 'openai-compatible']"
              :disable="!selectedWorkspaceId"
            />
            <q-input
              v-model="providerForm.api_base_url"
              outlined
              :label="t('onboarding.fields.apiBaseUrl')"
              :hint="t('onboarding.hints.provider')"
              :disable="!selectedWorkspaceId"
            />
            <q-select
              v-model="providerForm.profile_kind"
              outlined
              :label="t('onboarding.fields.profileKind')"
              :options="['chat', 'embedding']"
              :disable="!selectedWorkspaceId"
            />
            <q-input
              v-model="providerForm.model_name"
              outlined
              :label="t('onboarding.fields.modelName')"
              :disable="!selectedWorkspaceId"
            />
            <q-btn
              type="submit"
              color="primary"
              :disable="!selectedWorkspaceId"
              :label="t('onboarding.steps.provider.action')"
              :loading="
                providersStore.createAccountState.status === 'loading' ||
                  providersStore.createModelProfileState.status === 'loading'
              "
            />
          </q-form>
          <EmptyStateCard
            v-if="!selectedWorkspaceId"
            :title="t('onboarding.statuses.blocked')"
            :message="t('onboarding.empty.noWorkspace')"
          />
        </article>

        <article class="panel form-panel">
          <div class="panel__header">
            <div>
              <h3>{{ t('onboarding.steps.document.title') }}</h3>
              <p>{{ t('onboarding.steps.document.summary') }}</p>
            </div>
            <StatusBadge
              :status="
                (projectDocumentState?.documents.data.length ?? 0) > 0
                  ? 'Healthy'
                  : selectedProjectId
                    ? 'Pending'
                    : 'Blocked'
              "
              :label="
                (projectDocumentState?.documents.data.length ?? 0) > 0
                  ? t('onboarding.steps.document.complete')
                  : selectedProjectId
                    ? t('onboarding.statuses.pending')
                    : t('onboarding.statuses.blocked')
              "
            />
          </div>
          <q-form
            class="form-stack"
            @submit.prevent="submitDocument"
          >
            <q-select
              v-model="selectedProjectId"
              outlined
              emit-value
              map-options
              option-value="id"
              option-label="name"
              :label="t('common.project')"
              :options="projects"
              @update:model-value="syncSelectionDefaults"
            />
            <q-input
              v-model="documentForm.source_label"
              outlined
              :label="t('onboarding.fields.sourceLabel')"
              :disable="!selectedProjectId"
            />
            <q-select
              v-model="documentForm.source_kind"
              outlined
              :label="t('onboarding.fields.sourceKind')"
              :options="['upload', 'manual', 'seed']"
              :disable="!selectedProjectId"
            />
            <q-input
              v-model="documentForm.external_key"
              outlined
              :label="t('onboarding.fields.externalKey')"
              :disable="!selectedProjectId"
            />
            <q-input
              v-model="documentForm.title"
              outlined
              :label="t('onboarding.fields.documentTitle')"
              :disable="!selectedProjectId"
            />
            <q-input
              v-model="documentForm.text"
              outlined
              autogrow
              type="textarea"
              :label="t('onboarding.fields.documentText')"
              :hint="t('onboarding.hints.document')"
              :disable="!selectedProjectId"
            />
            <q-toggle
              v-model="documentForm.queueJob"
              :label="t('onboarding.fields.queueJob')"
              :disable="!selectedProjectId"
            />
            <q-btn
              type="submit"
              color="primary"
              :disable="!selectedProjectId"
              :label="t('onboarding.steps.document.action')"
              :loading="
                documentsStore.ingestState.status === 'loading' ||
                  documentsStore.createSourceState.status === 'loading' ||
                  documentsStore.createJobState.status === 'loading'
              "
            />
          </q-form>
          <EmptyStateCard
            v-if="!selectedProjectId"
            :title="t('onboarding.statuses.blocked')"
            :message="t('onboarding.empty.noProject')"
          />
        </article>
      </div>

      <div class="onboarding-grid onboarding-grid--bottom">
        <article class="panel">
          <div class="panel__header">
            <div>
              <h3>{{ t('onboarding.cards.readiness') }}</h3>
              <p>{{ currentProject?.name ?? t('common.notSelectedYet') }}</p>
            </div>
            <StatusBadge
              :status="readiness?.indexing_state ?? 'Pending'"
              :label="readiness?.indexing_state ?? t('onboarding.statuses.pending')"
            />
          </div>
          <EmptyStateCard
            v-if="!readiness"
            :title="t('onboarding.cards.readiness')"
            :message="t('onboarding.empty.noProject')"
          />
          <div
            v-else
            class="metric-grid"
          >
            <div>
              <span>{{ t('onboarding.metrics.readiness') }}</span>
              <strong>{{ readiness.ready_for_query ? t('common.yes') : t('common.no') }}</strong>
            </div>
            <div>
              <span>{{ t('onboarding.metrics.indexingState') }}</span>
              <strong>{{ readiness.indexing_state }}</strong>
            </div>
            <div>
              <span>{{ t('onboarding.metrics.sources') }}</span>
              <strong>{{ readiness.sources }}</strong>
            </div>
            <div>
              <span>{{ t('onboarding.metrics.documents') }}</span>
              <strong>{{ readiness.documents }}</strong>
            </div>
          </div>
        </article>

        <article class="panel">
          <div class="panel__header">
            <div>
              <h3>{{ t('onboarding.cards.connectedResources') }}</h3>
              <p>{{ currentWorkspace?.name ?? t('common.notSelectedYet') }}</p>
            </div>
            <StatusBadge
              :status="providerState ? 'Healthy' : 'Pending'"
              :label="providerState ? t('onboarding.statuses.active') : t('onboarding.statuses.pending')"
            />
          </div>
          <div class="resource-columns">
            <div>
              <h4>{{ t('providers.providerAccounts') }}</h4>
              <ul
                v-if="providerState?.provider_accounts.length"
                class="resource-list"
              >
                <li
                  v-for="account in providerState.provider_accounts"
                  :key="account.id"
                >
                  <span>{{ account.label }}</span>
                  <small>{{ account.provider_kind }}</small>
                </li>
              </ul>
              <EmptyStateCard
                v-else
                :title="t('providers.providerAccounts')"
                :message="t('onboarding.empty.noProvider')"
              />
            </div>
            <div>
              <h4>{{ t('documents.title') }}</h4>
              <ul
                v-if="projectDocumentState?.documents.data.length"
                class="resource-list"
              >
                <li
                  v-for="document in projectDocumentState.documents.data"
                  :key="document.id"
                >
                  <span>{{ document.title || document.external_key }}</span>
                  <small>{{ document.id }}</small>
                </li>
              </ul>
              <EmptyStateCard
                v-else
                :title="t('documents.title')"
                :message="t('onboarding.empty.noDocument')"
              />
            </div>
          </div>
        </article>
      </div>
    </template>
  </PageSection>
</template>

<style scoped>
.onboarding-hero {
  display: flex;
  justify-content: space-between;
  gap: var(--rr-space-6);
  padding: var(--rr-space-6);
  border: 1px solid var(--rr-color-border-subtle);
  border-radius: var(--rr-radius-lg);
  background:
    linear-gradient(135deg, rgb(37 99 235 / 0.08), rgb(255 255 255 / 0.95)),
    var(--rr-color-bg-surface);
  box-shadow: var(--rr-shadow-sm);
}

.onboarding-hero__copy {
  display: grid;
  gap: var(--rr-space-3);
  max-width: 62ch;
}

.onboarding-hero__copy h2,
.panel h3,
.panel h4,
.panel p {
  margin: 0;
}

.onboarding-hero__eyebrow {
  margin: 0;
  text-transform: uppercase;
  letter-spacing: 0.08em;
  font-size: 0.78rem;
  font-weight: 700;
  color: var(--rr-color-accent-700);
}

.result-chip {
  display: inline-grid;
  gap: 4px;
  padding: 12px 14px;
  border-radius: var(--rr-radius-sm);
  background: var(--rr-color-bg-surface);
  border: 1px solid var(--rr-color-border-subtle);
  width: fit-content;
}

.onboarding-grid,
.forms-grid {
  display: grid;
  gap: var(--rr-space-4);
}

.onboarding-grid {
  grid-template-columns: minmax(0, 1.6fr) minmax(320px, 0.9fr);
}

.onboarding-grid--bottom,
.forms-grid {
  grid-template-columns: repeat(2, minmax(0, 1fr));
}

.panel {
  display: grid;
  gap: var(--rr-space-4);
  padding: var(--rr-space-5);
  border-radius: var(--rr-radius-md);
  border: 1px solid var(--rr-color-border-subtle);
  background: var(--rr-color-bg-surface);
  box-shadow: var(--rr-shadow-sm);
}

.panel__header {
  display: flex;
  justify-content: space-between;
  gap: var(--rr-space-3);
  align-items: flex-start;
}

.checklist {
  display: grid;
  gap: var(--rr-space-3);
  margin: 0;
  padding: 0;
  list-style: none;
}

.checklist li {
  display: grid;
  grid-template-columns: auto minmax(0, 1fr) auto;
  gap: var(--rr-space-3);
  align-items: start;
  padding: var(--rr-space-4);
  border-radius: var(--rr-radius-sm);
  border: 1px solid var(--rr-color-border-subtle);
  background: var(--rr-color-bg-surface-muted);
}

.checklist li[data-complete='true'] {
  border-color: rgb(22 163 74 / 0.3);
  background: rgb(236 253 243 / 0.9);
}

.checklist__icon {
  display: grid;
  place-items: center;
  width: 32px;
  height: 32px;
  border-radius: 999px;
  background: rgb(255 255 255 / 0.92);
  color: var(--rr-color-accent-600);
}

.form-stack,
.resource-columns {
  display: grid;
  gap: var(--rr-space-3);
}

.metric-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: var(--rr-space-3);
}

.metric-grid div,
.resource-list li {
  display: grid;
  gap: 6px;
  padding: var(--rr-space-4);
  border-radius: var(--rr-radius-sm);
  background: var(--rr-color-bg-surface-muted);
}

.metric-grid span,
.resource-list small,
.checklist p,
.panel p {
  color: var(--rr-color-text-muted);
}

.resource-columns {
  grid-template-columns: repeat(2, minmax(0, 1fr));
}

.resource-list {
  display: grid;
  gap: var(--rr-space-3);
  margin: 0;
  padding: 0;
  list-style: none;
}

@media (width <= 1100px) {
  .onboarding-grid,
  .onboarding-grid--bottom,
  .forms-grid,
  .resource-columns,
  .metric-grid {
    grid-template-columns: 1fr;
  }
}

@media (width <= 780px) {
  .onboarding-hero,
  .panel__header {
    flex-direction: column;
  }
}
</style>
