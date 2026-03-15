<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { RouterLink } from 'vue-router'

import {
  fetchChatSessions,
  fetchProjects,
  fetchProjectReadiness,
  fetchWorkspaces,
  type ChatSessionSurface,
  type ProjectReadinessSummary,
} from 'src/boot/api'
import PageSection from 'src/components/shell/PageSection.vue'
import {
  getSelectedProjectId,
  getSelectedWorkspaceId,
  syncSelectedProjectId,
  syncSelectedWorkspaceId,
} from 'src/stores/flow'

interface WorkspaceItem {
  id: string
  slug: string
  name: string
}

interface ProjectItem {
  id: string
  slug: string
  name: string
}

const { t } = useI18n()

const workspaces = ref<WorkspaceItem[]>([])
const projects = ref<ProjectItem[]>([])
const readiness = ref<ProjectReadinessSummary | null>(null)
const recentSessions = ref<ChatSessionSurface[]>([])

const hasWorkspace = computed(() => workspaces.value.length > 0)
const hasProject = computed(() => projects.value.length > 0)
const selectedWorkspace = computed(
  () => workspaces.value.find((item) => item.id === getSelectedWorkspaceId()) ?? null,
)
const selectedProject = computed(
  () => projects.value.find((item) => item.id === getSelectedProjectId()) ?? null,
)
const indexedDocuments = computed(() => readiness.value?.documents ?? 0)
const hasReadyLibrary = computed(() => Boolean(readiness.value?.ready_for_query))
const latestSession = computed(() => recentSessions.value.at(0))

const primaryUploadAction = computed(() => ({
  to: hasProject.value ? '/files' : '/processing',
  label: hasProject.value
    ? t('flow.home.hero.actions.upload')
    : t('flow.home.hero.actions.prepareUpload'),
  hint: hasProject.value
    ? t('flow.home.hero.hints.uploadReady')
    : t('flow.home.hero.hints.uploadBlocked'),
}))

const nextStepCard = computed(() => {
  if (!hasWorkspace.value || !hasProject.value) {
    return {
      title: t('flow.home.nextSteps.prepare.title'),
      body: t('flow.home.nextSteps.prepare.body'),
      status: t('flow.home.nextSteps.prepare.status'),
      to: '/processing',
      action: t('flow.home.nextSteps.prepare.action'),
    }
  }

  if (hasReadyLibrary.value) {
    return {
      title: t('flow.home.nextSteps.ask.title'),
      body: t('flow.home.nextSteps.ask.body'),
      status: t('flow.home.nextSteps.ask.status'),
      to: '/search',
      action: t('flow.home.nextSteps.ask.action'),
    }
  }

  return {
    title: t('flow.home.nextSteps.processing.title'),
    body: t('flow.home.nextSteps.processing.body'),
    status:
      indexedDocuments.value > 0
        ? t('flow.home.nextSteps.processing.statusWorking')
        : t('flow.home.nextSteps.processing.statusWaiting'),
    to: '/files',
    action: t('flow.home.nextSteps.processing.action'),
  }
})

const uploadChecklist = computed(() => [
  {
    key: 'upload',
    title: t('flow.home.checklist.upload.title'),
    body: t('flow.home.checklist.upload.body'),
    done: hasProject.value && indexedDocuments.value > 0,
  },
  {
    key: 'processing',
    title: t('flow.home.checklist.processing.title'),
    body: t('flow.home.checklist.processing.body'),
    done: hasReadyLibrary.value,
  },
  {
    key: 'ask',
    title: t('flow.home.checklist.ask.title'),
    body: t('flow.home.checklist.ask.body'),
    done: hasReadyLibrary.value && Boolean(latestSession.value),
  },
])

const recentSessionStatus = computed(() => {
  if (!selectedProject.value) {
    return t('flow.home.sessions.blocked')
  }

  const session = latestSession.value
  if (!session) {
    return t('flow.home.sessions.empty')
  }

  return t('flow.home.sessions.ready', { count: session.message_count })
})

const recentSessionRoute = computed(() => {
  const session = latestSession.value
  return session ? `/search?session=${encodeURIComponent(session.id)}` : '/search'
})

const recentSessionUpdatedLabel = computed(() => {
  const session = latestSession.value
  return session ? formatDateTime(session.updated_at) : ''
})

onMounted(async () => {
  workspaces.value = await fetchWorkspaces()
  const activeWorkspaceId = syncSelectedWorkspaceId(workspaces.value)

  if (!activeWorkspaceId) {
    projects.value = []
    readiness.value = null
    recentSessions.value = []
    syncSelectedProjectId([])
    return
  }

  projects.value = await fetchProjects(activeWorkspaceId)
  const activeProjectId = syncSelectedProjectId(projects.value)

  if (!activeProjectId) {
    readiness.value = null
    recentSessions.value = []
    return
  }

  try {
    const [nextReadiness, nextSessions] = await Promise.all([
      fetchProjectReadiness(activeProjectId),
      fetchChatSessions(activeProjectId),
    ])
    readiness.value = nextReadiness
    recentSessions.value = nextSessions.slice(0, 3)
  } catch {
    readiness.value = null
    recentSessions.value = []
  }
})

function formatDateTime(value: string) {
  const date = new Date(value)

  if (Number.isNaN(date.getTime())) {
    return value
  }

  return date.toLocaleString()
}
</script>

<template>
  <section class="rr-page-grid home-page">
    <PageSection
      :title="t('flow.home.title')"
      :description="t('flow.home.description')"
      status="focused"
      :status-label="t('shell.status.focused')"
    >
      <template #actions>
        <RouterLink class="rr-button" :to="primaryUploadAction.to">
          {{ primaryUploadAction.label }}
        </RouterLink>
        <RouterLink class="rr-button rr-button--secondary" to="/search">
          {{ t('flow.home.hero.actions.ask') }}
        </RouterLink>
      </template>

      <article class="rr-panel rr-panel--accent upload-hero">
        <div class="upload-hero__copy">
          <p class="rr-kicker">{{ t('flow.home.hero.eyebrow') }}</p>
          <h2>{{ t('flow.home.hero.title') }}</h2>
          <p class="rr-note">{{ t('flow.home.hero.description') }}</p>
        </div>

        <div class="upload-hero__main-action">
          <RouterLink class="rr-button" :to="primaryUploadAction.to">
            {{ primaryUploadAction.label }}
          </RouterLink>
          <p class="rr-note">{{ primaryUploadAction.hint }}</p>
        </div>

        <div class="upload-hero__steps">
          <article class="upload-step-card">
            <span>1</span>
            <div>
              <strong>{{ t('flow.home.hero.steps.upload.title') }}</strong>
              <p>{{ t('flow.home.hero.steps.upload.body') }}</p>
            </div>
          </article>
          <article class="upload-step-card">
            <span>2</span>
            <div>
              <strong>{{ t('flow.home.hero.steps.processing.title') }}</strong>
              <p>{{ t('flow.home.hero.steps.processing.body') }}</p>
            </div>
          </article>
          <article class="upload-step-card">
            <span>3</span>
            <div>
              <strong>{{ t('flow.home.hero.steps.ask.title') }}</strong>
              <p>{{ t('flow.home.hero.steps.ask.body') }}</p>
            </div>
          </article>
        </div>
      </article>

      <div class="rr-stat-strip upload-status-strip">
        <article class="rr-stat">
          <p class="rr-stat__label">{{ t('flow.home.stats.documents') }}</p>
          <strong>{{ indexedDocuments }}</strong>
          <p>{{ t('flow.home.stats.documentsHint') }}</p>
        </article>
        <article class="rr-stat">
          <p class="rr-stat__label">{{ t('flow.home.stats.readiness') }}</p>
          <strong>{{
            hasReadyLibrary ? t('flow.home.stats.ready') : t('flow.home.stats.notReady')
          }}</strong>
          <p>
            {{
              hasReadyLibrary
                ? t('flow.home.stats.readinessHintReady')
                : t('flow.home.stats.readinessHintWaiting')
            }}
          </p>
        </article>
        <article class="rr-stat">
          <p class="rr-stat__label">{{ t('flow.home.stats.scope') }}</p>
          <strong>{{ selectedProject?.name ?? t('flow.common.empty') }}</strong>
          <p>{{ selectedWorkspace?.name ?? t('flow.common.empty') }}</p>
        </article>
      </div>

      <div class="home-core-grid">
        <article class="rr-panel rr-stack next-step-panel">
          <div class="next-step-panel__header">
            <div>
              <p class="rr-kicker">{{ t('flow.home.nextSteps.eyebrow') }}</p>
              <h3>{{ nextStepCard.title }}</h3>
            </div>
            <p class="next-step-panel__status">{{ nextStepCard.status }}</p>
          </div>

          <p class="rr-note">{{ nextStepCard.body }}</p>

          <RouterLink class="rr-button" :to="nextStepCard.to">
            {{ nextStepCard.action }}
          </RouterLink>
        </article>

        <article class="rr-panel rr-panel--muted rr-stack checklist-panel">
          <div>
            <p class="rr-kicker">{{ t('flow.home.checklist.eyebrow') }}</p>
            <h3>{{ t('flow.home.checklist.title') }}</h3>
          </div>

          <div class="checklist-items">
            <article
              v-for="item in uploadChecklist"
              :key="item.key"
              class="checklist-item"
              :data-done="item.done"
            >
              <div class="checklist-item__marker">{{ item.done ? '✓' : '•' }}</div>
              <div>
                <strong>{{ item.title }}</strong>
                <p>{{ item.body }}</p>
              </div>
            </article>
          </div>
        </article>
      </div>

      <div class="home-secondary-grid">
        <article class="rr-panel rr-panel--muted rr-stack session-panel">
          <div class="session-panel__header">
            <div>
              <p class="rr-kicker">{{ t('flow.home.sessions.eyebrow') }}</p>
              <h3>{{ t('flow.home.sessions.title') }}</h3>
            </div>
            <p class="session-panel__status">{{ recentSessionStatus }}</p>
          </div>

          <p class="rr-note">{{ t('flow.home.sessions.description') }}</p>

          <article class="session-card">
            <h4>{{ latestSession?.title || t('flow.home.sessions.fallbackTitle') }}</h4>
            <p>{{ latestSession?.last_message_preview || t('flow.home.sessions.emptyBody') }}</p>
            <dl v-if="latestSession" class="session-meta">
              <div>
                <dt>{{ t('flow.home.sessions.fields.updated') }}</dt>
                <dd>{{ recentSessionUpdatedLabel }}</dd>
              </div>
              <div>
                <dt>{{ t('flow.home.sessions.fields.messages') }}</dt>
                <dd>{{ latestSession.message_count }}</dd>
              </div>
            </dl>
            <div class="rr-action-row">
              <RouterLink class="rr-button rr-button--secondary" :to="recentSessionRoute">
                {{ latestSession ? t('flow.home.sessions.resume') : t('flow.home.sessions.start') }}
              </RouterLink>
              <RouterLink class="rr-button rr-button--ghost" to="/search">
                {{ t('flow.home.sessions.openAsk') }}
              </RouterLink>
            </div>
          </article>
        </article>

        <details class="rr-panel rr-panel--muted rr-stack admin-surfaces-panel">
          <summary class="admin-surfaces-panel__summary">
            <div>
              <p class="rr-kicker">{{ t('flow.home.secondaryEyebrow') }}</p>
              <h3>{{ t('flow.home.secondaryTitle') }}</h3>
            </div>
            <span>{{ t('flow.home.secondaryToggle') }}</span>
          </summary>

          <p class="rr-note">{{ t('flow.home.secondaryDescription') }}</p>

          <div class="admin-links">
            <RouterLink class="admin-link-card" to="/processing">
              <div>
                <strong>{{ t('flow.home.secondary.setup.title') }}</strong>
                <p>{{ t('flow.home.secondary.setup.body') }}</p>
              </div>
              <span>{{ t('flow.home.secondary.setup.action') }}</span>
            </RouterLink>
            <RouterLink class="admin-link-card" to="/graph">
              <div>
                <strong>{{ t('flow.home.secondary.graph.title') }}</strong>
                <p>{{ t('flow.home.secondary.graph.body') }}</p>
              </div>
              <span>{{ t('flow.home.secondary.graph.action') }}</span>
            </RouterLink>
            <RouterLink class="admin-link-card" to="/api">
              <div>
                <strong>{{ t('flow.home.secondary.api.title') }}</strong>
                <p>{{ t('flow.home.secondary.api.body') }}</p>
              </div>
              <span>{{ t('flow.home.secondary.api.action') }}</span>
            </RouterLink>
          </div>
        </details>
      </div>
    </PageSection>
  </section>
</template>

<style scoped>
.home-page,
.upload-hero,
.upload-hero__copy,
.upload-hero__steps,
.home-core-grid,
.home-secondary-grid,
.checklist-items,
.session-card,
.session-meta,
.admin-links {
  display: grid;
  gap: var(--rr-space-4);
}

.upload-hero {
  align-items: start;
}

.upload-hero__copy h2,
.next-step-panel__header h3,
.session-panel__header h3,
.admin-surfaces-panel__summary h3,
.session-card h4 {
  margin: 0;
}

.upload-hero__main-action,
.next-step-panel__header,
.session-panel__header,
.admin-surfaces-panel__summary,
.admin-link-card,
.checklist-item {
  display: flex;
  gap: var(--rr-space-3);
}

.upload-hero__main-action,
.next-step-panel__header,
.session-panel__header,
.admin-surfaces-panel__summary,
.admin-link-card {
  align-items: center;
  justify-content: space-between;
}

.upload-step-card,
.session-card,
.admin-link-card,
.checklist-item {
  padding: var(--rr-space-4);
  border: 1px solid var(--rr-color-border-subtle);
  border-radius: var(--rr-radius-lg);
  background: rgb(255 255 255 / 0.72);
}

.upload-hero__steps {
  grid-template-columns: repeat(3, minmax(0, 1fr));
}

.upload-step-card {
  display: grid;
  gap: var(--rr-space-3);
}

.upload-step-card span,
.checklist-item__marker {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 2rem;
  height: 2rem;
  border-radius: 999px;
  background: rgb(29 78 216 / 0.12);
  color: var(--rr-color-accent-700);
  font-weight: 700;
}

.upload-step-card p,
.checklist-item p,
.session-card p,
.admin-link-card p,
.session-meta dt,
.upload-status-strip p {
  margin: 0;
  color: var(--rr-color-text-secondary);
}

.upload-status-strip {
  grid-template-columns: repeat(3, minmax(0, 1fr));
}

.home-core-grid,
.home-secondary-grid {
  grid-template-columns: repeat(2, minmax(0, 1fr));
}

.next-step-panel__status,
.session-panel__status {
  margin: 0;
  font-size: 0.88rem;
  font-weight: 700;
  color: var(--rr-color-accent-700);
}

.checklist-item[data-done='true'] {
  border-color: rgb(21 128 61 / 0.28);
  background: rgb(240 253 244 / 0.8);
}

.session-meta {
  grid-template-columns: repeat(2, minmax(0, 1fr));
}

.session-meta dt,
.session-meta dd {
  margin: 0;
}

.admin-surfaces-panel__summary {
  cursor: pointer;
  list-style: none;
}

.admin-surfaces-panel__summary::-webkit-details-marker {
  display: none;
}

.admin-link-card {
  text-decoration: none;
  color: inherit;
}

.admin-link-card span {
  font-weight: 700;
  color: var(--rr-color-accent-700);
}

@media (width <= 900px) {
  .upload-hero__steps,
  .upload-status-strip,
  .home-core-grid,
  .home-secondary-grid,
  .session-meta {
    grid-template-columns: 1fr;
  }

  .upload-hero__main-action,
  .next-step-panel__header,
  .session-panel__header,
  .admin-surfaces-panel__summary,
  .admin-link-card {
    flex-direction: column;
    align-items: flex-start;
  }
}
</style>
