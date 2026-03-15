<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { RouterLink } from 'vue-router'

import {
  api,
  backendUrl,
  fetchProjects,
  fetchWorkspaces,
  fetchWorkspaceGovernance,
  isUnauthorizedApiError,
  type ProjectSummary,
  type WorkspaceGovernanceSummary,
  type WorkspaceSummary,
} from 'src/boot/api'
import CrossSurfaceGuide from 'src/components/shell/CrossSurfaceGuide.vue'
import PageSection from 'src/components/shell/PageSection.vue'
import ProductSpine from 'src/components/shell/ProductSpine.vue'
import StatusBadge from 'src/components/shell/StatusBadge.vue'
import EmptyStateCard from 'src/components/state/EmptyStateCard.vue'
import ErrorStateCard from 'src/components/state/ErrorStateCard.vue'
import LoadingSkeletonPanel from 'src/components/state/LoadingSkeletonPanel.vue'
import type { AppLocale } from 'src/i18n/messages'
import { getIntlLocale } from 'src/i18n/runtime'
import {
  clearApiBearerToken,
  getApiBearerToken,
  maskApiBearerToken,
  setApiBearerToken,
} from 'src/lib/apiAuth'
import { useIntegrationsStore } from 'src/stores/integrations'

interface TokenSummary {
  id: string
  workspace_id?: string | null
  token_kind: string
  label: string
  status: string
  scopes: string[]
  last_used_at?: string | null
  created_at: string
  updated_at: string
}

interface ExampleCard {
  key: string
  title: string
  description: string
  accessStatus: string
  accessLabel: string
  method: string
  path: string
  body: string
  notes: string[]
}

interface GroundworkCard {
  key: string
  title: string
  description: string
  status: string
  statusLabel: string
  notes: string[]
}

type ProtectedSurfaceState = 'missing_token' | 'ready' | 'unauthorized' | 'error'

const { t, locale } = useI18n()
const integrationsStore = useIntegrationsStore()

const loading = ref(true)
const refreshing = ref(false)
const errorMessage = ref<string | null>(null)

const workspaces = ref<WorkspaceSummary[]>([])
const selectedWorkspaceId = ref<string | null>(null)
const projects = ref<ProjectSummary[]>([])
const selectedProjectId = ref<string | null>(null)

const governance = ref<WorkspaceGovernanceSummary | null>(null)
const governanceState = ref<ProtectedSurfaceState>('missing_token')
const governanceError = ref<string | null>(null)

const tokens = ref<TokenSummary[]>([])
const tokenInventoryState = ref<ProtectedSurfaceState>('missing_token')
const tokenInventoryError = ref<string | null>(null)

const sessionTokenDraft = ref(getApiBearerToken())
const sessionToken = ref(getApiBearerToken())

const selectedWorkspace = computed(
  () => workspaces.value.find((workspace) => workspace.id === selectedWorkspaceId.value) ?? null,
)

const selectedProject = computed(
  () => projects.value.find((project) => project.id === selectedProjectId.value) ?? null,
)

const scopedProjects = computed(() => projects.value.slice(0, 6))
const hasSessionToken = computed(() => sessionToken.value.trim().length > 0)
const maskedSessionToken = computed(() =>
  hasSessionToken.value ? maskApiBearerToken(sessionToken.value) : null,
)

const showTechnicalApiInventory = ref(false)
const showTechnicalApiFoundation = ref(false)

const tokenScopeCounts = computed(() => {
  const counts = new Map<string, number>()
  tokens.value.forEach((token) => {
    token.scopes.forEach((scope) => {
      counts.set(scope, (counts.get(scope) ?? 0) + 1)
    })
  })

  return Array.from(counts.entries())
    .map(([scope, count]) => ({ scope, count }))
    .sort((left, right) => right.count - left.count || left.scope.localeCompare(right.scope))
})

const pageStatus = computed(() => {
  if (errorMessage.value) {
    return 'Failed'
  }

  if (loading.value || refreshing.value) {
    return 'Pending'
  }

  if (!selectedWorkspace.value) {
    return 'Warning'
  }

  if (hasSessionToken.value && (governanceState.value === 'ready' || tokenInventoryState.value === 'ready')) {
    return 'Healthy'
  }

  return 'Warning'
})

const pageStatusLabel = computed(() => {
  if (errorMessage.value) {
    return t('api.page.states.blocked')
  }

  if (loading.value || refreshing.value) {
    return t('api.page.states.loading')
  }

  if (!selectedWorkspace.value) {
    return t('api.page.states.foundation')
  }

  if (hasSessionToken.value && (governanceState.value === 'ready' || tokenInventoryState.value === 'ready')) {
    return t('api.page.states.ready')
  }

  return t('api.page.states.foundation')
})

const authPanelStatus = computed(() => {
  if (!hasSessionToken.value) {
    return {
      status: 'Warning',
      label: t('api.session.status.needsToken'),
    }
  }

  if (governanceState.value === 'error' || tokenInventoryState.value === 'error') {
    return {
      status: 'Warning',
      label: t('api.session.status.needsCheck'),
    }
  }

  if (governanceState.value === 'ready' || tokenInventoryState.value === 'ready') {
    return {
      status: 'Healthy',
      label: t('api.session.status.connected'),
    }
  }

  if (
    governanceState.value === 'unauthorized' ||
    tokenInventoryState.value === 'unauthorized'
  ) {
    return {
      status: 'Warning',
      label: t('api.session.status.limited'),
    }
  }

  return {
    status: 'Pending',
    label: t('api.session.status.verifying'),
  }
})

const launchpadCards = computed(() => [
  {
    label: t('api.launchpad.cards.endpoint'),
    value: backendUrl,
    hint: t('api.launchpad.cards.endpointHint'),
  },
  {
    label: t('api.launchpad.cards.auth'),
    value: hasSessionToken.value
      ? t('api.launchpad.cards.authConnected')
      : t('api.launchpad.cards.authMissing'),
    hint: hasSessionToken.value
      ? t('api.launchpad.cards.authHintReady', {
          token: maskedSessionToken.value ?? '',
        })
      : t('api.launchpad.cards.authHintMissing'),
  },
  {
    label: t('api.launchpad.cards.workspace'),
    value: selectedWorkspace.value?.slug ?? t('api.launchpad.cards.workspaceMissing'),
    hint: selectedWorkspace.value
      ? t('api.launchpad.cards.workspaceHintReady', {
          workspace: selectedWorkspace.value.name,
        })
      : t('api.launchpad.cards.workspaceHintMissing'),
  },
  {
    label: t('api.launchpad.cards.project'),
    value: selectedProject.value?.slug ?? t('api.launchpad.cards.projectWorkspaceWide'),
    hint: selectedProject.value
      ? t('api.launchpad.cards.projectHintReady', {
          project: selectedProject.value.name,
        })
      : t('api.launchpad.cards.projectHintMissing'),
  },
])

const startSteps = computed(() => [
  {
    key: 'endpoint',
    title: t('api.start.steps.endpoint.title'),
    description: t('api.start.steps.endpoint.description'),
    hint: backendUrl,
    status: 'Ready',
    statusLabel: t('api.start.status.ready'),
  },
  {
    key: 'token',
    title: t('api.start.steps.token.title'),
    description: t('api.start.steps.token.description'),
    hint: hasSessionToken.value
      ? t('api.start.steps.token.hintReady', {
          token: maskedSessionToken.value ?? '',
        })
      : t('api.start.steps.token.hintMissing'),
    status: hasSessionToken.value ? 'Ready' : 'Warning',
    statusLabel: hasSessionToken.value
      ? t('api.start.status.saved')
      : t('api.start.status.needsAction'),
  },
  {
    key: 'scope',
    title: t('api.start.steps.scope.title'),
    description: selectedWorkspace.value
      ? t('api.start.steps.scope.descriptionReady', {
          workspace: selectedWorkspace.value.name,
        })
      : t('api.start.steps.scope.description'),
    hint: selectedProject.value
      ? t('api.start.steps.scope.hintProject', {
          project: selectedProject.value.name,
        })
      : t('api.start.steps.scope.hintWorkspace'),
    status: selectedWorkspace.value ? 'Ready' : 'Warning',
    statusLabel: selectedWorkspace.value
      ? t('api.start.status.scoped')
      : t('api.start.status.needsSetup'),
  },
  {
    key: 'requests',
    title: t('api.start.steps.requests.title'),
    description: t('api.start.steps.requests.description'),
    hint: hasSessionToken.value
      ? t('api.start.steps.requests.hintWithToken')
      : t('api.start.steps.requests.hintWithoutToken'),
    status: selectedWorkspace.value ? 'Ready' : 'Pending',
    statusLabel: selectedWorkspace.value
      ? t('api.start.status.live')
      : t('api.start.status.waiting'),
  },
])

const protectedSurfaceChecklist = computed(() => [
  {
    key: 'governance',
    text: t('api.session.readiness.governance'),
    ...formatProtectedState(governanceState.value),
  },
  {
    key: 'tokens',
    text: t('api.session.readiness.tokens'),
    ...formatProtectedState(tokenInventoryState.value),
  },
])

const sessionNotes = computed(() => [
  t('api.session.notes.sessionOnly'),
  t('api.session.notes.mintingOutsideUi'),
  t('api.session.notes.plaintextOnce'),
])

const inventoryCards = computed(() => [
  {
    label: t('api.inventory.cards.workspaces'),
    value: String(workspaces.value.length),
    hint: t('api.inventory.cards.workspacesHint'),
  },
  {
    label: t('api.inventory.cards.projects'),
    value: String(projects.value.length),
    hint: t('api.inventory.cards.projectsHint'),
  },
  {
    label: t('api.inventory.cards.tokens'),
    value: tokenInventoryState.value === 'ready' ? String(tokens.value.length) : '—',
    hint:
      tokenInventoryState.value === 'ready'
        ? t('api.inventory.cards.tokensHint')
        : t('api.inventory.cards.tokensPendingHint'),
  },
  {
    label: t('api.inventory.cards.providerAccounts'),
    value: governanceState.value === 'ready' ? String(governance.value?.provider_accounts ?? 0) : '—',
    hint:
      governanceState.value === 'ready'
        ? t('api.inventory.cards.providerAccountsHint')
        : t('api.inventory.cards.protectedPendingHint'),
  },
  {
    label: t('api.inventory.cards.modelProfiles'),
    value: governanceState.value === 'ready' ? String(governance.value?.model_profiles ?? 0) : '—',
    hint:
      governanceState.value === 'ready'
        ? t('api.inventory.cards.modelProfilesHint')
        : t('api.inventory.cards.protectedPendingHint'),
  },
])

const foundationChecklist = computed(() => [
  {
    key: 'workspace',
    label: t('api.foundation.workspaceContext'),
    ready: Boolean(selectedWorkspace.value),
  },
  {
    key: 'token',
    label: t('api.foundation.sessionToken'),
    ready: hasSessionToken.value,
  },
  {
    key: 'project',
    label: t('api.foundation.projectScope'),
    ready: projects.value.length > 0,
  },
  {
    key: 'surface',
    label: t('api.foundation.protectedReadiness'),
    ready: governanceState.value === 'ready' || tokenInventoryState.value === 'ready',
  },
])

const projectGuidance = computed(() => {
  if (!selectedWorkspace.value) {
    return null
  }

  const workspaceSlug = selectedWorkspace.value.slug
  const projectSlug = selectedProject.value?.slug ?? 'your-project'
  const workspaceTokenLabel = tokens.value.find((token) => token.workspace_id === selectedWorkspace.value?.id)?.label
  const workspaceScopedToken = tokens.value.find(
    (token) => token.workspace_id === selectedWorkspace.value?.id && token.token_kind !== 'instance_admin',
  )

  return {
    title: selectedProject.value
      ? t('api.guidance.projectTitle', { project: selectedProject.value.name })
      : t('api.guidance.workspaceTitle', { workspace: selectedWorkspace.value.name }),
    description: selectedProject.value
      ? t('api.guidance.projectDescription', {
          workspace: selectedWorkspace.value.name,
          project: selectedProject.value.name,
        })
      : t('api.guidance.workspaceDescription', { workspace: selectedWorkspace.value.name }),
    bullets: [
      t('api.guidance.bullets.scopeWorkspace', { workspace: workspaceSlug }),
      t('api.guidance.bullets.scopeProject', { project: projectSlug }),
      workspaceTokenLabel
        ? t('api.guidance.bullets.tokenReuse', { token: workspaceTokenLabel })
        : t('api.guidance.bullets.tokenMissing'),
      workspaceScopedToken
        ? t('api.guidance.bullets.permissionsScoped')
        : t('api.guidance.bullets.permissionsAdminFallback'),
    ],
  }
})

const exampleCards = computed<ExampleCard[]>(() => {
  const workspaceId = selectedWorkspace.value?.id ?? '{workspace_id}'
  const projectId = selectedProject.value?.id ?? '{project_id}'
  const tokenExport = 'export RUSTRAG_TOKEN="rtrg_xxx_replace_me"'

  return [
    {
      key: 'health',
      title: t('api.examples.cards.health.title'),
      description: t('api.examples.cards.health.description'),
      accessStatus: 'Info',
      accessLabel: t('api.examples.access.public'),
      method: 'GET',
      path: '/v1/health',
      body: `curl -s ${backendUrl}/v1/health`,
      notes: [t('api.examples.cards.health.note')],
    },
    {
      key: 'projects',
      title: t('api.examples.cards.projects.title'),
      description: t('api.examples.cards.projects.description'),
      accessStatus: 'Info',
      accessLabel: t('api.examples.access.public'),
      method: 'GET',
      path: `/v1/projects?workspace_id=${workspaceId}`,
      body: `curl -s "${backendUrl}/v1/projects?workspace_id=${workspaceId}"`,
      notes: [t('api.examples.cards.projects.note')],
    },
    {
      key: 'workspace-governance',
      title: t('api.examples.cards.workspaceGovernance.title'),
      description: t('api.examples.cards.workspaceGovernance.description'),
      accessStatus: hasSessionToken.value ? 'Healthy' : 'Warning',
      accessLabel: t('api.examples.access.token'),
      method: 'GET',
      path: `/v1/workspaces/${workspaceId}/governance`,
      body: `${tokenExport}
curl -s ${backendUrl}/v1/workspaces/${workspaceId}/governance \\
  -H "Authorization: Bearer $RUSTRAG_TOKEN"`,
      notes: [
        t('api.examples.shared.tokenExport'),
        t('api.examples.cards.workspaceGovernance.note'),
      ],
    },
    {
      key: 'run-query',
      title: t('api.examples.cards.runQuery.title'),
      description: t('api.examples.cards.runQuery.description'),
      accessStatus: hasSessionToken.value ? 'Healthy' : 'Warning',
      accessLabel: t('api.examples.access.token'),
      method: 'POST',
      path: '/v1/query',
      body: `${tokenExport}
curl -s ${backendUrl}/v1/query \\
  -H "Authorization: Bearer $RUSTRAG_TOKEN" \\
  -H "Content-Type: application/json" \\
  -d '{
    "project_id": "${projectId}",
    "query_text": "What changed in the onboarding flow?",
    "top_k": 5
  }'`,
      notes: [
        t('api.examples.cards.runQuery.note'),
        t('api.examples.shared.workspaceScopeNote'),
      ],
    },
  ]
})

const groundworkCards = computed<GroundworkCard[]>(() => [
  {
    key: 'contract',
    title: t('api.groundwork.cards.contract.title'),
    description: t('api.groundwork.cards.contract.description'),
    status: 'Ready',
    statusLabel: t('api.groundwork.status.ready'),
    notes: [
      'backend/contracts/rustrag.openapi.yaml',
      t('api.groundwork.cards.contract.note'),
    ],
  },
  {
    key: 'types',
    title: t('api.groundwork.cards.types.title'),
    description: t('api.groundwork.cards.types.description'),
    status: 'Ready',
    statusLabel: t('api.groundwork.status.ready'),
    notes: [
      'frontend/src/contracts/api/generated.ts',
      'npm run api:generate',
    ],
  },
  {
    key: 'examples',
    title: t('api.groundwork.cards.examples.title'),
    description: t('api.groundwork.cards.examples.description'),
    status: 'Ready',
    statusLabel: t('api.groundwork.status.ready'),
    notes: [t('api.groundwork.cards.examples.note')],
  },
  {
    key: 'next',
    title: t('api.groundwork.cards.next.title'),
    description: t('api.groundwork.cards.next.description'),
    status: 'Blocked',
    statusLabel: t('api.groundwork.status.next'),
    notes: [t('api.groundwork.cards.next.note')],
  },
])

const endpointCards = computed(() => {
  const workspaceId = selectedWorkspace.value?.id ?? '{workspace_id}'
  const projectId = selectedProject.value?.id ?? '{project_id}'

  return integrationsStore.endpoints.map((endpoint) => ({
    ...endpoint,
    docs: [
      `${endpoint.baseUrl}/v1/health`,
      `${endpoint.baseUrl}/v1/workspaces`,
      `${endpoint.baseUrl}/v1/projects?workspace_id=${workspaceId}`,
      `${endpoint.baseUrl}/v1/workspaces/${workspaceId}/governance`,
      `${endpoint.baseUrl}/v1/query`,
      `${endpoint.baseUrl}/v1/documents?project_id=${projectId}`,
    ],
  }))
})

const tokenInventoryBadge = computed(() => {
  if (tokenInventoryState.value === 'ready' && tokens.value.length > 0) {
    return {
      status: 'Healthy',
      label: t('api.tokens.available'),
    }
  }

  if (tokenInventoryState.value === 'ready') {
    return {
      status: 'Info',
      label: t('api.tokens.emptyBadge'),
    }
  }

  return formatProtectedState(tokenInventoryState.value)
})

const tokenInventoryEmptyState = computed(() => {
  if (tokenInventoryState.value === 'missing_token') {
    return {
      title: t('api.tokens.missingTokenTitle'),
      message: t('api.tokens.missingTokenMessage'),
      hint: t('api.tokens.missingTokenHint'),
    }
  }

  if (tokenInventoryState.value === 'unauthorized') {
    return {
      title: t('api.tokens.unauthorizedTitle'),
      message: t('api.tokens.unauthorizedMessage'),
      hint: t('api.tokens.unauthorizedHint'),
    }
  }

  return {
    title: t('api.tokens.emptyTitle'),
    message: t('api.tokens.emptyMessage'),
    hint: t('api.tokens.emptyHint'),
  }
})

function formatProtectedState(state: ProtectedSurfaceState) {
  switch (state) {
    case 'ready':
      return {
        status: 'Healthy',
        label: t('api.session.readiness.ready'),
      }
    case 'unauthorized':
      return {
        status: 'Warning',
        label: t('api.session.readiness.unauthorized'),
      }
    case 'error':
      return {
        status: 'Warning',
        label: t('api.session.readiness.error'),
      }
    default:
      return {
        status: 'Warning',
        label: t('api.session.readiness.needsToken'),
      }
  }
}

function formatDate(value?: string | null) {
  if (!value) {
    return t('api.tokens.never')
  }

  const date = new Date(value)
  if (Number.isNaN(date.getTime())) {
    return value
  }

  return new Intl.DateTimeFormat(getIntlLocale(locale.value as AppLocale), {
    localeMatcher: 'best fit',
    dateStyle: 'medium',
    timeStyle: 'short',
  }).format(date)
}

function syncSelectedProject(projectItems: ProjectSummary[]) {
  if (projectItems.some((project) => project.id === selectedProjectId.value)) {
    return
  }

  selectedProjectId.value = projectItems[0]?.id ?? null
}

function resetProtectedSurface() {
  governance.value = null
  governanceError.value = null
  governanceState.value = hasSessionToken.value ? 'unauthorized' : 'missing_token'
  tokens.value = []
  tokenInventoryError.value = null
  tokenInventoryState.value = hasSessionToken.value ? 'unauthorized' : 'missing_token'
}

async function loadGovernance(workspaceId: string) {
  governance.value = null
  governanceError.value = null

  if (!hasSessionToken.value) {
    governanceState.value = 'missing_token'
    return
  }

  try {
    governance.value = await fetchWorkspaceGovernance(workspaceId)
    governanceState.value = 'ready'
  } catch (error) {
    if (isUnauthorizedApiError(error)) {
      governanceState.value = 'unauthorized'
      return
    }

    governanceState.value = 'error'
    governanceError.value = error instanceof Error ? error.message : t('api.page.errors.unknown')
  }
}

async function loadTokens(workspaceId: string) {
  tokens.value = []
  tokenInventoryError.value = null

  if (!hasSessionToken.value) {
    tokenInventoryState.value = 'missing_token'
    return
  }

  try {
    const { data } = await api.get<TokenSummary[]>('/auth/tokens', {
      params: { workspace_id: workspaceId },
    })
    tokens.value = data
    tokenInventoryState.value = 'ready'
  } catch (error) {
    if (isUnauthorizedApiError(error)) {
      tokenInventoryState.value = 'unauthorized'
      return
    }

    tokenInventoryState.value = 'error'
    tokenInventoryError.value = error instanceof Error ? error.message : t('api.page.errors.unknown')
  }
}

async function loadWorkspaceBundle(workspaceId: string) {
  const projectItems = await fetchProjects(workspaceId)
  projects.value = projectItems
  syncSelectedProject(projectItems)

  await Promise.all([loadGovernance(workspaceId), loadTokens(workspaceId)])
}

async function refreshProtectedSurface() {
  if (!selectedWorkspaceId.value) {
    resetProtectedSurface()
    return
  }

  await Promise.all([loadGovernance(selectedWorkspaceId.value), loadTokens(selectedWorkspaceId.value)])
}

async function refreshPage() {
  if (!selectedWorkspaceId.value) {
    governance.value = null
    projects.value = []
    tokens.value = []
    selectedProjectId.value = null
    return
  }

  refreshing.value = true
  errorMessage.value = null

  try {
    await loadWorkspaceBundle(selectedWorkspaceId.value)
  } catch (error) {
    errorMessage.value = error instanceof Error ? error.message : t('api.page.errors.unknown')
  } finally {
    refreshing.value = false
  }
}

async function selectWorkspace(workspaceId: string) {
  if (workspaceId === selectedWorkspaceId.value) {
    return
  }

  selectedWorkspaceId.value = workspaceId
  await refreshPage()
}

async function saveSessionToken() {
  setApiBearerToken(sessionTokenDraft.value)
  sessionToken.value = getApiBearerToken()
  sessionTokenDraft.value = sessionToken.value
  await refreshProtectedSurface()
}

async function clearSessionToken() {
  clearApiBearerToken()
  sessionToken.value = ''
  sessionTokenDraft.value = ''
  await refreshProtectedSurface()
}

onMounted(async () => {
  try {
    workspaces.value = await fetchWorkspaces()
    selectedWorkspaceId.value = workspaces.value[0]?.id ?? null

    if (selectedWorkspaceId.value) {
      await loadWorkspaceBundle(selectedWorkspaceId.value)
    } else {
      resetProtectedSurface()
    }
  } catch (error) {
    errorMessage.value = error instanceof Error ? error.message : t('api.page.errors.unknown')
  } finally {
    loading.value = false
  }
})
</script>

<template>
  <PageSection
    :eyebrow="t('api.page.eyebrow')"
    :title="t('api.page.title')"
    :description="t('api.page.description')"
    :status="pageStatus"
    :status-label="pageStatusLabel"
  >
    <template #actions>
      <RouterLink class="rr-button rr-button--secondary" to="/setup">
        {{ t('api.page.actions.setup') }}
      </RouterLink>
      <button
        type="button"
        class="rr-button"
        :disabled="loading || refreshing || !selectedWorkspaceId"
        @click="void refreshPage()"
      >
        {{ t('api.page.actions.refresh') }}
      </button>
    </template>

    <ProductSpine active-section="api" />
    <CrossSurfaceGuide active-section="api" />

    <LoadingSkeletonPanel
      v-if="loading"
      :title="t('api.page.loadingTitle')"
      :lines="8"
    />

    <ErrorStateCard
      v-else-if="errorMessage"
      :title="t('api.page.errors.title')"
      :message="errorMessage"
    />

    <EmptyStateCard
      v-else-if="!selectedWorkspace"
      :title="t('api.page.empty.noWorkspaceTitle')"
      :message="t('api.page.empty.noWorkspaceMessage')"
      :hint="t('api.page.empty.noWorkspaceHint')"
    />

    <template v-else>
      <section class="workspace-strip">
        <div>
          <p class="workspace-strip__eyebrow">{{ t('api.workspace.eyebrow') }}</p>
          <h2>{{ selectedWorkspace.name }}</h2>
          <p>{{ t('api.workspace.description', { slug: selectedWorkspace.slug }) }}</p>
          <p class="workspace-strip__note">{{ t('api.page.technicalNote') }}</p>
        </div>

        <div class="workspace-strip__stack">
          <StatusBadge
            :status="authPanelStatus.status"
            :label="authPanelStatus.label"
          />

          <div
            v-if="workspaces.length > 1"
            class="workspace-strip__actions"
          >
            <button
              v-for="workspace in workspaces"
              :key="workspace.id"
              type="button"
              class="workspace-pill"
              :class="{ 'workspace-pill--active': workspace.id === selectedWorkspaceId }"
              @click="void selectWorkspace(workspace.id)"
            >
              <span>{{ workspace.name }}</span>
              <small>{{ workspace.slug }}</small>
            </button>
          </div>
        </div>
      </section>

      <div class="summary-grid summary-grid--launchpad">
        <article
          v-for="card in launchpadCards"
          :key="card.label"
          class="summary-card"
        >
          <span>{{ card.label }}</span>
          <strong class="summary-card__value">{{ card.value }}</strong>
          <small>{{ card.hint }}</small>
        </article>
      </div>

      <div class="launchpad-grid">
        <article class="panel panel--start">
          <div class="panel__header">
            <div>
              <p class="panel__eyebrow">{{ t('api.start.eyebrow') }}</p>
              <h3>{{ t('api.start.title') }}</h3>
              <p>{{ t('api.start.description') }}</p>
            </div>
          </div>

          <div class="step-grid">
            <article
              v-for="(step, index) in startSteps"
              :key="step.key"
              class="step-card"
            >
              <div class="step-card__header">
                <span class="step-card__index">{{ index + 1 }}</span>
                <StatusBadge
                  :status="step.status"
                  :label="step.statusLabel"
                />
              </div>
              <h4>{{ step.title }}</h4>
              <p>{{ step.description }}</p>
              <small>{{ step.hint }}</small>
            </article>
          </div>

          <div class="launchpad-actions">
            <RouterLink class="rr-button rr-button--secondary" to="/setup">
              {{ t('api.start.actions.setup') }}
            </RouterLink>
            <RouterLink class="rr-button rr-button--secondary" to="/ingest">
              {{ t('api.start.actions.ingest') }}
            </RouterLink>
          </div>
        </article>

        <article class="panel">
          <div class="panel__header">
            <div>
              <p class="panel__eyebrow">{{ t('api.session.eyebrow') }}</p>
              <h3>{{ t('api.session.title') }}</h3>
              <p>{{ t('api.session.description') }}</p>
            </div>
            <StatusBadge
              :status="authPanelStatus.status"
              :label="authPanelStatus.label"
            />
          </div>

          <div class="rr-form-grid">
            <label class="rr-field">
              <span class="rr-field__label">{{ t('api.session.label') }}</span>
              <input
                v-model="sessionTokenDraft"
                class="rr-control"
                type="password"
                :placeholder="t('api.session.placeholder')"
                autocomplete="off"
              >
            </label>
          </div>

          <div class="rr-action-row">
            <button
              type="button"
              class="rr-button"
              :disabled="!sessionTokenDraft.trim()"
              @click="void saveSessionToken()"
            >
              {{ t('api.session.actions.save') }}
            </button>
            <button
              type="button"
              class="rr-button rr-button--secondary"
              :disabled="!hasSessionToken"
              @click="void clearSessionToken()"
            >
              {{ t('api.session.actions.clear') }}
            </button>
          </div>

          <article class="session-card">
            <div>
              <p class="panel__eyebrow">{{ t('api.session.activeLabel') }}</p>
              <h4>{{ maskedSessionToken ?? t('api.session.activeNone') }}</h4>
              <p>
                {{
                  hasSessionToken
                    ? t('api.session.activeDescription')
                    : t('api.session.missingDescription')
                }}
              </p>
            </div>
            <StatusBadge
              :status="authPanelStatus.status"
              :label="authPanelStatus.label"
            />
          </article>

          <ul class="checklist">
            <li
              v-for="item in protectedSurfaceChecklist"
              :key="item.key"
              class="checklist__item"
            >
              <StatusBadge
                :status="item.status"
                :label="item.label"
              />
              <span>{{ item.text }}</span>
            </li>
          </ul>

          <ul class="session-notes">
            <li
              v-for="note in sessionNotes"
              :key="note"
            >
              {{ note }}
            </li>
          </ul>
        </article>
      </div>

      <div class="content-grid">
        <article class="panel panel--examples">
          <div class="panel__header">
            <div>
              <p class="panel__eyebrow">{{ t('api.examples.eyebrow') }}</p>
              <h3>{{ t('api.examples.title') }}</h3>
              <p>{{ t('api.examples.description') }}</p>
            </div>
            <StatusBadge
              status="Info"
              :label="t('api.examples.liveSurface')"
            />
          </div>

          <div class="examples-grid">
            <article
              v-for="example in exampleCards"
              :key="example.key"
              class="example-card"
            >
              <div class="example-card__header">
                <div>
                  <h4>{{ example.title }}</h4>
                  <p>{{ example.description }}</p>
                </div>
                <div class="example-card__meta">
                  <StatusBadge
                    :status="example.accessStatus"
                    :label="example.accessLabel"
                  />
                  <div class="method-chip">
                    <strong>{{ example.method }}</strong>
                    <small>{{ example.path }}</small>
                  </div>
                </div>
              </div>

              <pre><code>{{ example.body }}</code></pre>

              <ul class="example-notes">
                <li
                  v-for="note in example.notes"
                  :key="note"
                >
                  {{ note }}
                </li>
              </ul>
            </article>
          </div>
        </article>

        <article class="panel">
          <div class="panel__header">
            <div>
              <p class="panel__eyebrow">{{ t('api.guidance.eyebrow') }}</p>
              <h3>{{ t('api.guidance.title') }}</h3>
              <p>{{ t('api.guidance.description') }}</p>
            </div>
            <StatusBadge
              :status="selectedProject ? 'Healthy' : 'Info'"
              :label="selectedProject ? t('api.guidance.projectScoped') : t('api.guidance.workspaceScoped')"
            />
          </div>

          <div class="project-picker">
            <button
              type="button"
              class="project-pill"
              :class="{ 'project-pill--active': selectedProjectId === null }"
              @click="selectedProjectId = null"
            >
              <span>{{ t('api.guidance.allProjects') }}</span>
              <small>{{ selectedWorkspace.slug }}</small>
            </button>
            <button
              v-for="project in scopedProjects"
              :key="project.id"
              type="button"
              class="project-pill"
              :class="{ 'project-pill--active': project.id === selectedProjectId }"
              @click="selectedProjectId = project.id"
            >
              <span>{{ project.name }}</span>
              <small>{{ project.slug }}</small>
            </button>
          </div>

          <div
            v-if="projectGuidance"
            class="guidance-card"
          >
            <h4>{{ projectGuidance.title }}</h4>
            <p>{{ projectGuidance.description }}</p>
            <ul class="guidance-list">
              <li
                v-for="bullet in projectGuidance.bullets"
                :key="bullet"
              >
                {{ bullet }}
              </li>
            </ul>
          </div>
        </article>
      </div>

      <div class="inventory-grid">
        <article class="panel">
          <div class="panel__header">
            <div>
              <p class="panel__eyebrow">{{ t('api.inventory.eyebrow') }}</p>
              <h3>{{ t('api.inventory.title') }}</h3>
              <p>{{ t('api.inventory.description') }}</p>
            </div>
            <StatusBadge
              :status="governanceState === 'ready' || tokenInventoryState === 'ready' ? 'Healthy' : 'Warning'"
              :label="
                governanceState === 'ready' || tokenInventoryState === 'ready'
                  ? t('api.inventory.ready')
                  : t('api.inventory.needsToken')
              "
            />
          </div>

          <div class="summary-grid">
            <article
              v-for="card in inventoryCards"
              :key="card.label"
              class="summary-card"
            >
              <span>{{ card.label }}</span>
              <strong class="summary-card__value">{{ card.value }}</strong>
              <small>{{ card.hint }}</small>
            </article>
          </div>

          <details class="technical-panel" :open="showTechnicalApiFoundation">
            <summary @click.prevent="showTechnicalApiFoundation = !showTechnicalApiFoundation">
              <span>{{ t('api.inventory.technicalSummary') }}</span>
              <small>{{ t('api.inventory.technicalHint') }}</small>
            </summary>

            <ul class="checklist">
              <li
                v-for="item in foundationChecklist"
                :key="item.key"
                class="checklist__item"
              >
                <StatusBadge
                  :status="item.ready ? 'Healthy' : 'Warning'"
                  :label="item.ready ? t('api.foundation.ready') : t('api.foundation.todo')"
                />
                <span>{{ item.label }}</span>
              </li>
            </ul>

            <p
              v-if="governanceError"
              class="panel-note"
            >
              {{ governanceError }}
            </p>
          </details>
        </article>

        <article class="panel">
          <div class="panel__header">
            <div>
              <p class="panel__eyebrow">{{ t('api.tokens.eyebrow') }}</p>
              <h3>{{ t('api.tokens.title') }}</h3>
              <p>{{ t('api.tokens.description') }}</p>
            </div>
            <StatusBadge
              :status="tokenInventoryBadge.status"
              :label="tokenInventoryBadge.label"
            />
          </div>

          <ul
            v-if="tokenInventoryState === 'ready' && tokens.length > 0"
            class="token-list"
          >
            <li
              v-for="token in tokens"
              :key="token.id"
              class="token-card"
            >
              <div class="token-card__header">
                <div>
                  <strong>{{ token.label }}</strong>
                  <p>{{ token.token_kind }} · {{ token.id.slice(0, 8) }}</p>
                </div>
                <StatusBadge
                  :status="token.status"
                />
              </div>

              <div class="token-card__meta">
                <span>{{ t('api.tokens.createdAt') }}: {{ formatDate(token.created_at) }}</span>
                <span>{{ t('api.tokens.lastUsedAt') }}: {{ formatDate(token.last_used_at) }}</span>
              </div>

              <div class="scope-cloud">
                <span
                  v-for="scope in token.scopes"
                  :key="`${token.id}-${scope}`"
                  class="scope-pill"
                >
                  {{ scope }}
                </span>
              </div>
            </li>
          </ul>

          <ErrorStateCard
            v-else-if="tokenInventoryState === 'error'"
            :title="t('api.tokens.errorTitle')"
            :message="tokenInventoryError ?? t('api.page.errors.unknown')"
          />

          <EmptyStateCard
            v-else
            :title="tokenInventoryEmptyState.title"
            :message="tokenInventoryEmptyState.message"
            :hint="tokenInventoryEmptyState.hint"
          />

          <details
            v-if="tokenScopeCounts.length > 0"
            class="technical-panel scope-inventory"
            :open="showTechnicalApiInventory"
          >
            <summary @click.prevent="showTechnicalApiInventory = !showTechnicalApiInventory">
              <span>{{ t('api.tokens.scopeInventoryTitle') }}</span>
              <small>{{ t('api.tokens.scopeInventoryDescription') }}</small>
            </summary>
            <div class="scope-cloud">
              <span
                v-for="item in tokenScopeCounts"
                :key="item.scope"
                class="scope-pill scope-pill--counted"
              >
                {{ item.scope }}
                <strong>{{ item.count }}</strong>
              </span>
            </div>
          </details>
        </article>
      </div>

      <div class="content-grid">
        <article class="panel">
          <div class="panel__header">
            <div>
              <p class="panel__eyebrow">{{ t('api.groundwork.eyebrow') }}</p>
              <h3>{{ t('api.groundwork.title') }}</h3>
              <p>{{ t('api.groundwork.description') }}</p>
            </div>
          </div>

          <div class="groundwork-grid">
            <article
              v-for="card in groundworkCards"
              :key="card.key"
              class="groundwork-card"
            >
              <div class="groundwork-card__header">
                <div>
                  <h4>{{ card.title }}</h4>
                  <p>{{ card.description }}</p>
                </div>
                <StatusBadge
                  :status="card.status"
                  :label="card.statusLabel"
                />
              </div>

              <ul class="groundwork-notes">
                <li
                  v-for="note in card.notes"
                  :key="note"
                >
                  {{ note }}
                </li>
              </ul>
            </article>
          </div>
        </article>

        <article class="panel">
          <div class="panel__header">
            <div>
              <p class="panel__eyebrow">{{ t('api.endpoints.eyebrow') }}</p>
              <h3>{{ t('api.endpoints.title') }}</h3>
              <p>{{ t('api.endpoints.description') }}</p>
            </div>
            <StatusBadge
              :status="integrationsStore.hasConfiguredEndpoints ? 'Healthy' : 'Warning'"
              :label="
                integrationsStore.hasConfiguredEndpoints
                  ? t('api.endpoints.configured')
                  : t('api.endpoints.unconfigured')
              "
            />
          </div>

          <div class="endpoint-grid">
            <article
              v-for="endpoint in endpointCards"
              :key="endpoint.key"
              class="endpoint-card"
            >
              <div class="endpoint-card__header">
                <div>
                  <h4>{{ endpoint.label }}</h4>
                  <p>{{ endpoint.baseUrl }}</p>
                </div>
              <StatusBadge
                :status="endpoint.status === 'configured' ? 'Healthy' : 'Warning'"
                :label="t(`common.status.${endpoint.status}`)"
              />
              </div>

              <ul class="endpoint-links">
                <li
                  v-for="doc in endpoint.docs"
                  :key="doc"
                >
                  <code>{{ doc }}</code>
                </li>
              </ul>
            </article>
          </div>
        </article>
      </div>
    </template>
  </PageSection>
</template>

<style scoped>
.launchpad-grid,
.inventory-grid,
.content-grid {
  display: grid;
  gap: var(--rr-space-4);
  align-items: start;
}

.launchpad-grid {
  grid-template-columns: minmax(0, 1.15fr) minmax(320px, 0.85fr);
}

.inventory-grid,
.content-grid {
  grid-template-columns: repeat(2, minmax(0, 1fr));
}

.panel {
  display: grid;
  gap: var(--rr-space-4);
  padding: var(--rr-space-5);
  border: 1px solid var(--rr-color-border-subtle);
  border-radius: var(--rr-radius-md);
  background: var(--rr-color-bg-surface);
}

.technical-panel {
  display: grid;
  gap: var(--rr-space-3);
  padding: var(--rr-space-4);
  border: 1px dashed var(--rr-color-border-subtle);
  border-radius: var(--rr-radius-md);
  background: rgb(248 250 252 / 0.7);
}

.technical-panel summary {
  display: grid;
  gap: 0.25rem;
  cursor: pointer;
  list-style: none;
}

.technical-panel summary::-webkit-details-marker {
  display: none;
}

.panel--start,
.panel--examples {
  min-height: 100%;
}

.panel__header {
  display: flex;
  justify-content: space-between;
  gap: var(--rr-space-3);
  align-items: flex-start;
}

.panel__eyebrow,
.workspace-strip__eyebrow {
  margin: 0 0 6px;
  font-size: 0.78rem;
  font-weight: 700;
  letter-spacing: 0.08em;
  text-transform: uppercase;
  color: var(--rr-color-accent-600);
}

.panel h3,
.panel h4,
.panel p,
.workspace-strip h2,
.workspace-strip p,
.scope-inventory__header h4,
.scope-inventory__header small {
  margin: 0;
}

.workspace-strip {
  display: flex;
  justify-content: space-between;
  gap: var(--rr-space-4);
  padding: var(--rr-space-5);
  border: 1px solid var(--rr-color-border-subtle);
  border-radius: var(--rr-radius-md);
  background: linear-gradient(145deg, rgb(239 246 255 / 0.96), rgb(248 250 252 / 0.98));
}

.workspace-strip__stack {
  display: grid;
  justify-items: end;
  gap: var(--rr-space-3);
}

.workspace-strip__note {
  max-width: 48rem;
  margin-top: var(--rr-space-2);
  color: var(--rr-color-text-secondary);
}

.workspace-strip__actions,
.project-picker,
.launchpad-actions {
  display: flex;
  flex-wrap: wrap;
  gap: var(--rr-space-2);
}

.workspace-pill,
.project-pill {
  display: grid;
  gap: 2px;
  min-width: 140px;
  padding: 12px 14px;
  border: 1px solid var(--rr-color-border-strong);
  border-radius: var(--rr-radius-sm);
  background: #fff;
  color: inherit;
  text-align: left;
  cursor: pointer;
  transition:
    border-color 0.18s ease,
    box-shadow 0.18s ease,
    transform 0.18s ease;
}

.workspace-pill:hover,
.project-pill:hover {
  border-color: #93c5fd;
  box-shadow: var(--rr-shadow-sm);
  transform: translateY(-1px);
}

.workspace-pill small,
.project-pill small,
.summary-card small,
.token-card p,
.token-card__meta,
.example-card p,
.guidance-card p,
.endpoint-card p,
.groundwork-card p,
.scope-inventory__header small,
.step-card p,
.step-card small,
.session-card p,
.panel-note {
  color: var(--rr-color-text-secondary);
}

.workspace-pill--active,
.project-pill--active {
  border-color: var(--rr-color-accent-600);
  background: var(--rr-color-accent-50);
  box-shadow: var(--rr-shadow-md);
}

.summary-grid,
.step-grid,
.examples-grid,
.endpoint-grid,
.groundwork-grid {
  display: grid;
  gap: var(--rr-space-3);
}

.summary-grid {
  grid-template-columns: repeat(auto-fit, minmax(160px, 1fr));
}

.summary-grid--launchpad {
  grid-template-columns: repeat(auto-fit, minmax(220px, 1fr));
}

.step-grid {
  grid-template-columns: repeat(auto-fit, minmax(220px, 1fr));
}

.examples-grid,
.endpoint-grid,
.groundwork-grid {
  grid-template-columns: repeat(auto-fit, minmax(260px, 1fr));
}

.summary-card,
.token-card,
.example-card,
.guidance-card,
.endpoint-card,
.groundwork-card,
.checklist__item,
.scope-inventory,
.step-card,
.session-card {
  border: 1px solid var(--rr-color-border-subtle);
  border-radius: var(--rr-radius-sm);
  background: #fff;
}

.summary-card,
.guidance-card,
.scope-inventory,
.endpoint-card,
.example-card,
.groundwork-card,
.step-card,
.session-card {
  padding: var(--rr-space-4);
}

.summary-card,
.step-card {
  display: grid;
  gap: 6px;
}

.summary-card__value {
  font-size: 1.05rem;
  line-height: 1.3;
  word-break: break-word;
}

.step-card__header,
.token-card__header,
.example-card__header,
.endpoint-card__header,
.groundwork-card__header,
.scope-inventory__header,
.session-card {
  display: flex;
  justify-content: space-between;
  gap: var(--rr-space-2);
  align-items: flex-start;
}

.step-card__index {
  display: inline-grid;
  place-items: center;
  width: 28px;
  height: 28px;
  border-radius: 999px;
  background: var(--rr-color-accent-50);
  color: var(--rr-color-accent-700);
  font-size: 0.88rem;
  font-weight: 700;
}

.checklist,
.token-list,
.guidance-list,
.example-notes,
.endpoint-links,
.session-notes,
.groundwork-notes {
  display: grid;
  gap: var(--rr-space-2);
  margin: 0;
  padding: 0;
  list-style: none;
}

.checklist__item {
  display: flex;
  gap: var(--rr-space-2);
  align-items: center;
  padding: 12px 14px;
}

.token-card {
  display: grid;
  gap: var(--rr-space-3);
  padding: var(--rr-space-4);
}

.token-card__meta {
  display: flex;
  flex-wrap: wrap;
  gap: 10px 16px;
  font-size: 0.88rem;
}

.session-card {
  padding: var(--rr-space-4);
}

.session-notes,
.groundwork-notes,
.example-notes {
  padding-left: 18px;
  list-style: disc;
}

.session-notes li,
.example-notes li {
  color: var(--rr-color-text-secondary);
}

.scope-cloud {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
}

.scope-pill,
.method-chip {
  display: inline-flex;
  gap: 8px;
  align-items: center;
  padding: 7px 10px;
  border-radius: 999px;
  background: var(--rr-color-accent-50);
  color: var(--rr-color-accent-700);
  font-size: 0.88rem;
}

.scope-pill--counted {
  background: var(--rr-color-bg-surface-muted);
  color: var(--rr-color-text-primary);
}

.scope-pill strong {
  color: var(--rr-color-accent-700);
}

.method-chip {
  flex-direction: column;
  align-items: flex-end;
  border-radius: var(--rr-radius-sm);
}

.example-card__meta {
  display: grid;
  justify-items: end;
  gap: 8px;
}

pre {
  margin: 0;
  padding: var(--rr-space-4);
  overflow-x: auto;
  border-radius: var(--rr-radius-sm);
  background: var(--rr-color-text-primary);
  color: #e2e8f0;
}

code {
  font-family: 'JetBrains Mono', 'SFMono-Regular', ui-monospace, monospace;
}

.endpoint-links code {
  display: block;
  padding: 10px 12px;
  border-radius: 12px;
  background: var(--rr-color-bg-surface-muted);
  color: var(--rr-color-text-primary);
}

.panel-note {
  margin: 0;
  font-size: 0.9rem;
}

@media (width <= 1180px) {
  .launchpad-grid,
  .inventory-grid,
  .content-grid {
    grid-template-columns: 1fr;
  }
}

@media (width <= 900px) {
  .workspace-strip,
  .panel__header,
  .token-card__header,
  .example-card__header,
  .endpoint-card__header,
  .groundwork-card__header,
  .scope-inventory__header,
  .session-card {
    flex-direction: column;
  }

  .workspace-strip__stack,
  .example-card__meta {
    justify-items: start;
  }
}
</style>
