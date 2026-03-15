<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { RouterLink } from 'vue-router'

import {
  fetchChatSessions,
  fetchProjectReadiness,
  type ChatSessionSurface,
  type ProjectReadinessSummary,
} from 'src/boot/api'
import PageSection from 'src/components/shell/PageSection.vue'
import { getSelectedProjectId, getSelectedWorkspaceId } from 'src/stores/flow'
import { hydrateWorkspaceProjectScope } from 'src/lib/productFlow'

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
  const scope = await hydrateWorkspaceProjectScope({
    setWorkspaces: (items) => {
      workspaces.value = items
    },
    setProjects: (items) => {
      projects.value = items
    },
  })

  if (!scope.workspaceId || !scope.projectId) {
    readiness.value = null
    recentSessions.value = []
    return
  }

  try {
    const [nextReadiness, nextSessions] = await Promise.all([
      fetchProjectReadiness(scope.projectId),
      fetchChatSessions(scope.projectId),
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
        <RouterLink
          class="rr-button"
          :to="primaryUploadAction.to"
        >
          {{ primaryUploadAction.label }}
        </RouterLink>
        <RouterLink
          class="rr-button rr-button--secondary"
          to="/search"
        >
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
          <RouterLink
            class="rr-button"
            :to="primaryUploadAction.to"
          >
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

      <div class="home-secondary-grid home-secondary-grid--single">
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
            <dl
              v-if="latestSession"
              class="session-meta"
            >
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
              <RouterLink
                class="rr-button rr-button--secondary"
                :to="recentSessionRoute"
              >
                {{ latestSession ? t('flow.home.sessions.resume') : t('flow.home.sessions.start') }}
              </RouterLink>
              <RouterLink
                class="rr-button rr-button--ghost"
                to="/search"
              >
                {{ t('flow.home.sessions.openAsk') }}
              </RouterLink>
            </div>
          </article>
        </article>
      </div>
    </PageSection>
  </section>
</template>

<style scoped>
.home-page,
.upload-hero,
.upload-hero__copy,
.upload-hero__steps,
.home-secondary-grid,
.session-card,
.session-meta {
  display: grid;
  gap: var(--rr-space-4);
}

.home-secondary-grid--single {
  grid-template-columns: minmax(0, 1fr);
}

.session-panel {
  max-width: 48rem;
}

.upload-hero {
  align-items: start;
}

.upload-hero__copy h2,
.session-panel__header h3,
.session-card h4 {
  margin: 0;
}

.upload-hero__main-action,
.session-panel__header {
  display: flex;
  gap: var(--rr-space-3);
  align-items: center;
  justify-content: space-between;
}

.upload-step-card,
.session-card {
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

.upload-step-card span {
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
.session-card p,
.session-meta dt,
.upload-status-strip p {
  margin: 0;
  color: var(--rr-color-text-secondary);
}

.upload-status-strip {
  grid-template-columns: repeat(3, minmax(0, 1fr));
}

.home-secondary-grid {
  grid-template-columns: repeat(2, minmax(0, 1fr));
}

.session-panel__status {
  margin: 0;
  font-size: 0.88rem;
  font-weight: 700;
  color: var(--rr-color-accent-700);
}

.session-card .rr-action-row {
  flex-wrap: wrap;
}

.session-card .rr-button {
  text-decoration: none;
}

.session-card .rr-button--ghost {
  padding-inline: 0;
}

.session-card .rr-button--ghost:hover {
  padding-inline: 0;
}

.session-meta {
  grid-template-columns: repeat(2, minmax(0, 1fr));
}

.session-meta dt,
.session-meta dd {
  margin: 0;
}

@media (width <= 900px) {
  .upload-hero__steps,
  .upload-status-strip,
  .home-secondary-grid,
  .session-meta {
    grid-template-columns: 1fr;
  }

  .upload-hero__main-action,
  .session-panel__header {
    flex-direction: column;
    align-items: flex-start;
  }
}
</style>
