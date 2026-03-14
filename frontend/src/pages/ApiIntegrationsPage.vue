<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'

import {
  api,
  backendUrl,
  fetchProjects,
  fetchWorkspaces,
  fetchWorkspaceGovernance,
  type ProjectSummary,
  type WorkspaceGovernanceSummary,
  type WorkspaceSummary,
} from 'src/boot/api'
import PageSection from 'src/components/shell/PageSection.vue'
import StatusBadge from 'src/components/shell/StatusBadge.vue'
import EmptyStateCard from 'src/components/state/EmptyStateCard.vue'
import ErrorStateCard from 'src/components/state/ErrorStateCard.vue'
import LoadingSkeletonPanel from 'src/components/state/LoadingSkeletonPanel.vue'
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
  method: string
  path: string
  body: string
  notes: string[]
}

const { t } = useI18n()
const integrationsStore = useIntegrationsStore()

const loading = ref(true)
const refreshing = ref(false)
const errorMessage = ref<string | null>(null)

const workspaces = ref<WorkspaceSummary[]>([])
const selectedWorkspaceId = ref<string | null>(null)
const governance = ref<WorkspaceGovernanceSummary | null>(null)
const projects = ref<ProjectSummary[]>([])
const selectedProjectId = ref<string | null>(null)
const tokens = ref<TokenSummary[]>([])

const selectedWorkspace = computed(() =>
  workspaces.value.find((workspace) => workspace.id === selectedWorkspaceId.value) ?? null,
)

const selectedProject = computed(() =>
  projects.value.find((project) => project.id === selectedProjectId.value) ?? null,
)

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

const scopedProjects = computed(() => projects.value.slice(0, 6))

const pageStatus = computed(() => {
  if (errorMessage.value) {
    return 'Failed'
  }

  if (loading.value || refreshing.value) {
    return 'Pending'
  }

  if (tokens.value.length === 0 || projects.value.length === 0) {
    return 'Warning'
  }

  return 'Healthy'
})

const pageStatusLabel = computed(() => {
  if (errorMessage.value) {
    return t('api.page.states.blocked')
  }

  if (loading.value || refreshing.value) {
    return t('api.page.states.loading')
  }

  if (tokens.value.length === 0 || projects.value.length === 0) {
    return t('api.page.states.foundation')
  }

  return t('api.page.states.ready')
})

const inventoryCards = computed(() => [
  {
    label: t('api.inventory.cards.tokens'),
    value: String(tokens.value.length),
    hint: t('api.inventory.cards.tokensHint'),
  },
  {
    label: t('api.inventory.cards.projects'),
    value: String(projects.value.length),
    hint: t('api.inventory.cards.projectsHint'),
  },
  {
    label: t('api.inventory.cards.providerAccounts'),
    value: String(governance.value?.provider_accounts ?? 0),
    hint: t('api.inventory.cards.providerAccountsHint'),
  },
  {
    label: t('api.inventory.cards.modelProfiles'),
    value: String(governance.value?.model_profiles ?? 0),
    hint: t('api.inventory.cards.modelProfilesHint'),
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
    label: t('api.foundation.tokenInventory'),
    ready: tokens.value.length > 0,
  },
  {
    key: 'project',
    label: t('api.foundation.projectScope'),
    ready: projects.value.length > 0,
  },
  {
    key: 'provider',
    label: t('api.foundation.providerReadiness'),
    ready: (governance.value?.provider_accounts ?? 0) > 0 && (governance.value?.model_profiles ?? 0) > 0,
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

const endpointCards = computed(() => {
  const workspaceId = selectedWorkspace.value?.id ?? '{workspace_id}'
  const projectId = selectedProject.value?.id ?? '{project_id}'

  return integrationsStore.endpoints.map((endpoint) => ({
    ...endpoint,
    docs: [
      `${endpoint.baseUrl}/v1/workspaces`,
      `${endpoint.baseUrl}/v1/provider-governance/${workspaceId}`,
      `${endpoint.baseUrl}/v1/projects?workspace_id=${workspaceId}`,
      `${endpoint.baseUrl}/v1/query`,
      `${endpoint.baseUrl}/v1/documents?project_id=${projectId}`,
    ],
  }))
})

const exampleCards = computed<ExampleCard[]>(() => {
  const workspaceId = selectedWorkspace.value?.id ?? '{workspace_id}'
  const projectId = selectedProject.value?.id ?? '{project_id}'
  const tokenLabel = tokens.value[0]?.label ?? 'workspace-token'
  const tokenValue = 'rtrg_xxx_replace_me'
  const projectName = selectedProject.value?.name ?? 'Example Project'

  return [
    {
      key: 'workspace-governance',
      title: t('api.examples.cards.workspaceGovernance.title'),
      description: t('api.examples.cards.workspaceGovernance.description'),
      method: 'GET',
      path: `/v1/workspaces/${workspaceId}/governance`,
      body: `curl -s ${backendUrl}/v1/workspaces/${workspaceId}/governance \\
  -H "Authorization: Bearer ${tokenValue}"`,
      notes: [
        t('api.examples.shared.tokenNote', { token: tokenLabel }),
        t('api.examples.cards.workspaceGovernance.note'),
      ],
    },
    {
      key: 'project-documents',
      title: t('api.examples.cards.projectDocuments.title'),
      description: t('api.examples.cards.projectDocuments.description', { project: projectName }),
      method: 'GET',
      path: `/v1/documents?project_id=${projectId}`,
      body: `curl -s "${backendUrl}/v1/documents?project_id=${projectId}" \\
  -H "Authorization: Bearer ${tokenValue}"`,
      notes: [
        t('api.examples.cards.projectDocuments.note'),
        t('api.examples.shared.workspaceScopeNote'),
      ],
    },
    {
      key: 'run-query',
      title: t('api.examples.cards.runQuery.title'),
      description: t('api.examples.cards.runQuery.description'),
      method: 'POST',
      path: '/v1/query',
      body: `curl -s ${backendUrl}/v1/query \\
  -H "Authorization: Bearer ${tokenValue}" \\
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

function formatDate(value?: string | null) {
  if (!value) {
    return t('api.tokens.never')
  }

  const date = new Date(value)
  if (Number.isNaN(date.getTime())) {
    return value
  }

  return new Intl.DateTimeFormat(undefined, {
    dateStyle: 'medium',
    timeStyle: 'short',
  }).format(date)
}

async function loadTokens(workspaceId: string) {
  const { data } = await api.get<TokenSummary[]>('/v1/auth/tokens', {
    params: { workspace_id: workspaceId },
  })
  tokens.value = data
}

async function loadWorkspaceBundle(workspaceId: string) {
  const [governanceData, projectItems] = await Promise.all([
    fetchWorkspaceGovernance(workspaceId),
    fetchProjects(workspaceId),
    loadTokens(workspaceId),
  ])

  governance.value = governanceData
  projects.value = projectItems
  selectedProjectId.value = projectItems[0]?.id ?? null
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

function selectWorkspace(workspaceId: string) {
  if (workspaceId === selectedWorkspaceId.value) {
    return
  }

  selectedWorkspaceId.value = workspaceId
}

watch(selectedWorkspaceId, (workspaceId, previousId) => {
  if (!workspaceId || workspaceId === previousId) {
    return
  }

  void refreshPage()
})

onMounted(async () => {
  try {
    workspaces.value = await fetchWorkspaces()
    selectedWorkspaceId.value = workspaces.value[0]?.id ?? null

    if (selectedWorkspaceId.value) {
      await loadWorkspaceBundle(selectedWorkspaceId.value)
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
      <button
        type="button"
        class="ghost-button"
        :disabled="loading || refreshing || !selectedWorkspaceId"
        @click="refreshPage"
      >
        {{ t('api.page.actions.refresh') }}
      </button>
    </template>

    <LoadingSkeletonPanel
      v-if="loading"
      :title="t('api.page.loadingTitle')"
      :lines="6"
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
        </div>

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
            @click="selectWorkspace(workspace.id)"
          >
            <span>{{ workspace.name }}</span>
            <small>{{ workspace.slug }}</small>
          </button>
        </div>
      </section>

      <div class="inventory-grid">
        <article class="panel panel--inventory">
          <div class="panel__header">
            <div>
              <p class="panel__eyebrow">{{ t('api.inventory.eyebrow') }}</p>
              <h3>{{ t('api.inventory.title') }}</h3>
              <p>{{ t('api.inventory.description') }}</p>
            </div>
            <StatusBadge
              :status="tokens.length > 0 ? 'Healthy' : 'Warning'"
              :label="tokens.length > 0 ? t('api.inventory.ready') : t('api.inventory.needsTokens')"
            />
          </div>

          <div class="summary-grid">
            <div
              v-for="card in inventoryCards"
              :key="card.label"
              class="summary-card"
            >
              <span>{{ card.label }}</span>
              <strong>{{ card.value }}</strong>
              <small>{{ card.hint }}</small>
            </div>
          </div>

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
        </article>

        <article class="panel">
          <div class="panel__header">
            <div>
              <p class="panel__eyebrow">{{ t('api.tokens.eyebrow') }}</p>
              <h3>{{ t('api.tokens.title') }}</h3>
              <p>{{ t('api.tokens.description') }}</p>
            </div>
            <StatusBadge
              :status="tokens.length > 0 ? 'Healthy' : 'Warning'"
              :label="tokens.length > 0 ? t('api.tokens.available') : t('api.tokens.emptyBadge')"
            />
          </div>

          <ul
            v-if="tokens.length > 0"
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
                  <p>
                    {{ token.token_kind }} · {{ token.id.slice(0, 8) }}
                  </p>
                </div>
                <StatusBadge
                  :status="token.status"
                  :label="token.status"
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
          <EmptyStateCard
            v-else
            :title="t('api.tokens.emptyTitle')"
            :message="t('api.tokens.emptyMessage')"
            :hint="t('api.tokens.emptyHint')"
          />

          <div
            v-if="tokenScopeCounts.length > 0"
            class="scope-inventory"
          >
            <div class="scope-inventory__header">
              <h4>{{ t('api.tokens.scopeInventoryTitle') }}</h4>
              <small>{{ t('api.tokens.scopeInventoryDescription') }}</small>
            </div>
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
          </div>
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
              status="Healthy"
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
                <div class="method-chip">
                  <strong>{{ example.method }}</strong>
                  <small>{{ example.path }}</small>
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
              :status="selectedProject ? 'Healthy' : 'Warning'"
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

      <article class="panel panel--endpoints">
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
                :label="endpoint.status"
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
    </template>
  </PageSection>
</template>

<style scoped>
.inventory-grid,
.content-grid {
  display: grid;
  gap: var(--rr-space-4);
  align-items: start;
}

.inventory-grid {
  grid-template-columns: minmax(0, 0.9fr) minmax(0, 1.1fr);
}

.content-grid {
  grid-template-columns: minmax(0, 1.2fr) minmax(280px, 0.8fr);
}

.panel {
  display: grid;
  gap: var(--rr-space-4);
  padding: var(--rr-space-5);
  border: 1px solid var(--rr-color-border-subtle);
  border-radius: var(--rr-radius-md);
  background: var(--rr-color-bg-surface);
}

.panel--inventory,
.panel--examples,
.panel--endpoints {
  grid-column: 1 / -1;
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
  background: linear-gradient(135deg, rgb(239 246 255 / 0.96), rgb(248 250 252 / 0.98));
}

.workspace-strip__actions,
.project-picker {
  display: flex;
  flex-wrap: wrap;
  gap: var(--rr-space-2);
}

.workspace-pill,
.project-pill,
.ghost-button {
  border: 1px solid var(--rr-color-border-strong);
  background: #fff;
  color: inherit;
  cursor: pointer;
  transition:
    border-color 0.18s ease,
    box-shadow 0.18s ease,
    transform 0.18s ease;
}

.workspace-pill:hover,
.project-pill:hover,
.ghost-button:hover {
  border-color: #93c5fd;
  box-shadow: var(--rr-shadow-sm);
  transform: translateY(-1px);
}

.workspace-pill,
.project-pill {
  display: grid;
  gap: 2px;
  padding: 12px 14px;
  min-width: 140px;
  border-radius: var(--rr-radius-sm);
  text-align: left;
}

.workspace-pill small,
.project-pill small,
.summary-card small,
.token-card p,
.token-card__meta,
.example-card p,
.guidance-card p,
.endpoint-card p,
.scope-inventory__header small {
  color: var(--rr-color-text-secondary);
}

.workspace-pill--active,
.project-pill--active {
  border-color: var(--rr-color-accent-600);
  background: var(--rr-color-accent-50);
  box-shadow: var(--rr-shadow-md);
}

.ghost-button {
  padding: 10px 14px;
  border-radius: 999px;
}

.ghost-button:disabled {
  cursor: not-allowed;
  opacity: 0.6;
  box-shadow: none;
  transform: none;
}

.summary-grid,
.examples-grid,
.endpoint-grid {
  display: grid;
  gap: var(--rr-space-3);
}

.summary-grid {
  grid-template-columns: repeat(auto-fit, minmax(160px, 1fr));
}

.examples-grid,
.endpoint-grid {
  grid-template-columns: repeat(auto-fit, minmax(260px, 1fr));
}

.summary-card,
.token-card,
.example-card,
.guidance-card,
.endpoint-card,
.checklist__item,
.scope-inventory {
  border: 1px solid var(--rr-color-border-subtle);
  border-radius: var(--rr-radius-sm);
  background: #fff;
}

.summary-card,
.guidance-card,
.scope-inventory,
.endpoint-card,
.example-card {
  padding: var(--rr-space-4);
}

.summary-card {
  display: grid;
  gap: 6px;
}

.summary-card strong {
  font-size: 1.1rem;
}

.checklist,
.token-list,
.guidance-list,
.example-notes,
.endpoint-links {
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

.token-card__header,
.example-card__header,
.endpoint-card__header,
.scope-inventory__header {
  display: flex;
  justify-content: space-between;
  gap: var(--rr-space-2);
  align-items: flex-start;
}

.token-card__meta {
  display: flex;
  flex-wrap: wrap;
  gap: 10px 16px;
  font-size: 0.88rem;
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

@media (width <= 1100px) {
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
  .scope-inventory__header {
    flex-direction: column;
  }
}
</style>
