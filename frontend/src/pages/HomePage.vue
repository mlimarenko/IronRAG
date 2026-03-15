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
import AppPanel from 'src/components/ui/AppPanel.vue'
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
const nextAction = computed(() => {
  if (!hasWorkspace.value) {
    return t('flow.home.next.setup')
  }

  if (!hasProject.value) {
    return t('flow.home.next.project')
  }

  if (hasReadyLibrary.value) {
    return t('flow.home.next.ask')
  }

  if (indexedDocuments.value > 0) {
    return t('flow.home.next.finishIndexing')
  }

  return t('flow.home.next.files')
})
const heroAction = computed(() => {
  if (!hasWorkspace.value || !hasProject.value) {
    return {
      to: '/processing',
      label: t('flow.home.hero.actions.setup'),
      secondaryTo: '/files',
      secondaryLabel: t('flow.home.hero.actions.filesSecondary'),
    }
  }

  if (hasReadyLibrary.value) {
    return {
      to: '/search',
      label: t('flow.home.hero.actions.ask'),
      secondaryTo: '/files',
      secondaryLabel: t('flow.home.hero.actions.filesSecondary'),
    }
  }

  return {
    to: '/files',
    label: t('flow.home.hero.actions.files'),
    secondaryTo: '/processing',
    secondaryLabel: t('flow.home.hero.actions.setupSecondary'),
  }
})
const productCards = computed(() => [
  {
    title: t('flow.home.cards.home.title'),
    body: t('flow.home.cards.home.body'),
    status: nextAction.value,
    to: '/home',
    action: t('flow.home.cards.home.action'),
  },
  {
    title: t('flow.home.cards.files.title'),
    body: t('flow.home.cards.files.body'),
    status: hasProject.value ? t('flow.home.cards.files.ready') : t('flow.home.cards.files.blocked'),
    to: '/files',
    action: t('flow.home.cards.files.action'),
  },
  {
    title: t('flow.home.cards.ask.title'),
    body: t('flow.home.cards.ask.body'),
    status: hasReadyLibrary.value ? t('flow.home.cards.ask.ready') : t('flow.home.cards.ask.blocked'),
    to: '/search',
    action: t('flow.home.cards.ask.action'),
  },
])
const secondaryCards = computed(() => [
  {
    title: t('flow.home.secondary.graph.title'),
    body: t('flow.home.secondary.graph.body'),
    to: '/graph',
    action: t('flow.home.secondary.graph.action'),
  },
  {
    title: t('flow.home.secondary.api.title'),
    body: t('flow.home.secondary.api.body'),
    to: '/api',
    action: t('flow.home.secondary.api.action'),
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
        <RouterLink class="rr-button" :to="heroAction.to">
          {{ heroAction.label }}
        </RouterLink>
        <RouterLink class="rr-button rr-button--secondary" :to="heroAction.secondaryTo">
          {{ heroAction.secondaryLabel }}
        </RouterLink>
      </template>

      <article class="rr-panel rr-panel--accent home-hero">
        <div class="home-hero__copy">
          <p class="rr-kicker">{{ t('flow.home.hero.eyebrow') }}</p>
          <h2>{{ t('flow.home.hero.title') }}</h2>
          <p class="rr-note">{{ t('flow.home.hero.description') }}</p>
        </div>

        <div class="home-hero__stats rr-stat-strip">
          <article class="rr-stat">
            <p class="rr-stat__label">{{ t('flow.home.stats.workspace') }}</p>
            <strong>{{ selectedWorkspace?.name ?? t('flow.common.empty') }}</strong>
          </article>
          <article class="rr-stat">
            <p class="rr-stat__label">{{ t('flow.home.stats.project') }}</p>
            <strong>{{ selectedProject?.name ?? t('flow.common.empty') }}</strong>
          </article>
          <article class="rr-stat">
            <p class="rr-stat__label">{{ t('flow.home.stats.documents') }}</p>
            <strong>{{ indexedDocuments }}</strong>
          </article>
          <article class="rr-stat">
            <p class="rr-stat__label">{{ t('flow.home.stats.next') }}</p>
            <strong>{{ nextAction }}</strong>
          </article>
        </div>
      </article>

      <div class="home-primary-grid">
        <AppPanel
          v-for="card in productCards"
          :key="card.to"
          class="home-card"
          tone="muted"
          :eyebrow="t('flow.home.primaryEyebrow')"
          :title="card.title"
        >
          <p class="home-card__body">{{ card.body }}</p>
          <p class="home-card__status">{{ card.status }}</p>
          <RouterLink class="rr-button rr-button--secondary" :to="card.to">
            {{ card.action }}
          </RouterLink>
        </AppPanel>
      </div>

      <AppPanel
        class="home-session-panel"
        tone="muted"
        :eyebrow="t('flow.home.sessions.eyebrow')"
        :title="t('flow.home.sessions.title')"
      >
        <p class="rr-note">{{ t('flow.home.sessions.description') }}</p>

        <div class="home-session-grid">
          <article class="home-session-card">
            <p class="home-card__status">{{ recentSessionStatus }}</p>
            <h3>
              {{ latestSession?.title || t('flow.home.sessions.fallbackTitle') }}
            </h3>
            <p class="home-session-card__preview">
              {{ latestSession?.last_message_preview || t('flow.home.sessions.emptyBody') }}
            </p>
            <dl v-if="latestSession" class="home-session-meta">
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
              <RouterLink v-if="selectedProject" class="rr-button rr-button--ghost" to="/search">
                {{ t('flow.home.sessions.openAsk') }}
              </RouterLink>
            </div>
          </article>

          <div v-if="recentSessions.length > 1" class="home-session-list">
            <article
              v-for="session in recentSessions.slice(1)"
              :key="session.id"
              class="home-session-list__item"
            >
              <div>
                <strong>{{ session.title || t('flow.home.sessions.fallbackTitle') }}</strong>
                <p>{{ session.last_message_preview || t('flow.home.sessions.emptyBody') }}</p>
              </div>
              <RouterLink
                class="rr-button rr-button--secondary"
                :to="`/search?session=${encodeURIComponent(session.id)}`"
              >
                {{ t('flow.home.sessions.resume') }}
              </RouterLink>
            </article>
          </div>
        </div>
      </AppPanel>

      <AppPanel
        class="home-secondary-panel"
        tone="muted"
        :eyebrow="t('flow.home.secondaryEyebrow')"
        :title="t('flow.home.secondaryTitle')"
      >
        <p class="rr-note">{{ t('flow.home.secondaryDescription') }}</p>

        <div class="home-secondary-grid">
          <article v-for="card in secondaryCards" :key="card.to" class="home-secondary-card">
            <div>
              <h3>{{ card.title }}</h3>
              <p>{{ card.body }}</p>
            </div>
            <RouterLink class="rr-button rr-button--secondary" :to="card.to">
              {{ card.action }}
            </RouterLink>
          </article>
        </div>
      </AppPanel>
    </PageSection>
  </section>
</template>

<style scoped>
.home-page,
.home-primary-grid,
.home-secondary-grid,
.home-secondary-card,
.home-card,
.home-hero,
.home-session-grid,
.home-session-card,
.home-session-list {
  gap: var(--rr-space-4);
}

.home-hero__copy,
.home-session-meta,
.home-session-list__item {
  display: grid;
  gap: 0.45rem;
}

.home-hero__copy h2,
.home-secondary-card h3,
.home-session-card h3 {
  margin: 0;
}

.home-card__body,
.home-card__status,
.home-secondary-card p,
.home-session-card__preview,
.home-session-list__item p {
  margin: 0;
}

.home-card__body,
.home-secondary-card p,
.home-session-card__preview,
.home-session-list__item p,
.home-session-meta dt {
  color: var(--rr-color-text-secondary);
}

.home-card__status {
  font-size: 0.88rem;
  font-weight: 700;
  color: var(--rr-color-accent-700);
}

.home-primary-grid,
.home-session-grid {
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
}

.home-session-card {
  grid-column: span 2;
}

.home-session-card,
.home-session-list__item,
.home-secondary-card {
  padding: var(--rr-space-4);
  border: 1px solid var(--rr-color-border-subtle);
  border-radius: var(--rr-radius-lg);
  background: rgb(255 255 255 / 0.7);
}

.home-session-meta {
  grid-template-columns: repeat(2, minmax(0, 1fr));
}

.home-session-meta dt,
.home-session-meta dd {
  margin: 0;
}

.home-session-list {
  display: grid;
}

.home-session-list__item {
  align-content: space-between;
}

.home-secondary-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
}

@media (width <= 900px) {
  .home-primary-grid,
  .home-secondary-grid,
  .home-session-grid,
  .home-session-meta {
    grid-template-columns: 1fr;
  }

  .home-session-card {
    grid-column: span 1;
  }
}
</style>
