<script setup lang="ts">
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'

import {
  createBootstrapToken,
  isBootstrapNotConfiguredApiError,
  isUnauthorizedApiError,
} from 'src/boot/api'
import StatusBadge from 'src/components/shell/StatusBadge.vue'
import {
  clearApiBearerToken,
  getApiBearerToken,
  maskApiBearerToken,
  setApiBearerToken,
} from 'src/lib/apiAuth'

const DEFAULT_BOOTSTRAP_SCOPES = [
  'workspace:admin',
  'projects:write',
  'providers:admin',
  'documents:read',
  'documents:write',
  'query:run',
  'usage:read',
]

const { t } = useI18n()

withDefaults(
  defineProps<{
    title?: string
    description?: string
    contextNote?: string | null
  }>(),
  {
    title: undefined,
    description: undefined,
    contextNote: null,
  },
)

const emit = defineEmits<{
  updated: []
}>()

const sessionTokenDraft = ref(getApiBearerToken())
const sessionToken = ref(getApiBearerToken())
const bootstrapSecret = ref('')
const loading = ref(false)
const feedback = ref<{
  tone: 'success' | 'warning'
  message: string
  detail?: string | null
} | null>(null)

const hasSessionToken = computed(() => sessionToken.value.trim().length > 0)
const maskedSessionToken = computed(() => {
  const token = sessionToken.value
  return token ? maskApiBearerToken(token) : null
})
const panelStatus = computed(() =>
  hasSessionToken.value
    ? { status: 'Healthy', label: t('api.session.status.connected') }
    : { status: 'Warning', label: t('api.session.status.needsToken') },
)
const hasDraftToken = computed(() => sessionTokenDraft.value.trim().length > 0)
const hasBootstrapSecret = computed(() => bootstrapSecret.value.trim().length > 0)
const showAdvancedAccess = ref(false)
const stepItems = computed(() => [
  {
    key: 'auth',
    title: t('flow.processing.auth.cards.auth.title'),
    body: t('flow.processing.auth.cards.auth.body'),
    hint: hasSessionToken.value
      ? t('flow.processing.auth.cards.auth.hintReady', { token: maskedSessionToken.value ?? '' })
      : t('flow.processing.auth.cards.auth.hintPending'),
    status: hasSessionToken.value ? 'Healthy' : 'Warning',
    label: hasSessionToken.value
      ? t('flow.processing.auth.cards.auth.ready')
      : t('flow.processing.auth.cards.auth.pending'),
  },
  {
    key: 'setup',
    title: t('flow.processing.auth.cards.setup.title'),
    body: t('flow.processing.auth.cards.setup.body'),
    hint: t('flow.processing.auth.cards.setup.hint'),
    status: hasSessionToken.value ? 'Healthy' : 'Info',
    label: hasSessionToken.value
      ? t('flow.processing.auth.cards.setup.ready')
      : t('flow.processing.auth.cards.setup.pending'),
  },
  {
    key: 'next',
    title: t('flow.processing.auth.cards.next.title'),
    body: t('flow.processing.auth.cards.next.body'),
    hint: t('flow.processing.auth.cards.next.hint'),
    status: hasSessionToken.value ? 'Healthy' : 'Info',
    label: hasSessionToken.value
      ? t('flow.processing.auth.cards.next.ready')
      : t('flow.processing.auth.cards.next.pending'),
  },
])

function refreshSessionDraft() {
  sessionToken.value = getApiBearerToken()
  sessionTokenDraft.value = sessionToken.value
}

function setFeedback(tone: 'success' | 'warning', message: string, detail: string | null = null) {
  feedback.value = {
    tone,
    message,
    detail,
  }
}

function humanizeBootstrapError(error: unknown) {
  if (isBootstrapNotConfiguredApiError(error)) {
    return {
      message: t('flow.processing.auth.states.bootstrapUnavailable.title'),
      detail: t('flow.processing.auth.states.bootstrapUnavailable.body'),
    }
  }

  if (isUnauthorizedApiError(error)) {
    return {
      message: t('flow.processing.auth.states.bootstrapRejected.title'),
      detail: t('flow.processing.auth.states.bootstrapRejected.body'),
    }
  }

  const rawMessage = error instanceof Error ? error.message : t('api.page.errors.unknown')
  const normalized = rawMessage.toLowerCase()

  if (normalized.includes('404') || normalized.includes('not found')) {
    return {
      message: t('flow.processing.auth.states.bootstrapMissing.title'),
      detail: t('flow.processing.auth.states.bootstrapMissing.body'),
    }
  }

  if (normalized.includes('failed to fetch') || normalized.includes('network')) {
    return {
      message: t('flow.processing.auth.states.bootstrapNetwork.title'),
      detail: t('flow.processing.auth.states.bootstrapNetwork.body'),
    }
  }

  return {
    message: t('flow.processing.auth.states.bootstrapFailed.title'),
    detail: rawMessage,
  }
}

function saveSessionToken() {
  setApiBearerToken(sessionTokenDraft.value)
  refreshSessionDraft()
  setFeedback(
    hasSessionToken.value ? 'success' : 'warning',
    hasSessionToken.value
      ? t('flow.processing.auth.states.tokenSaved.title')
      : t('flow.processing.auth.states.tokenCleared.title'),
    hasSessionToken.value
      ? t('flow.processing.auth.states.tokenSaved.body')
      : t('flow.processing.auth.states.tokenCleared.body'),
  )
  emit('updated')
}

function toggleAdvancedAccess() {
  showAdvancedAccess.value = !showAdvancedAccess.value
}

function clearSessionToken() {
  clearApiBearerToken()
  refreshSessionDraft()
  setFeedback(
    'warning',
    t('flow.processing.auth.states.tokenCleared.title'),
    t('flow.processing.auth.states.tokenCleared.body'),
  )
  emit('updated')
}

async function mintBootstrapSessionToken() {
  const secret = bootstrapSecret.value.trim()
  if (!secret) {
    setFeedback(
      'warning',
      t('flow.processing.auth.states.secretMissing.title'),
      t('flow.processing.auth.states.secretMissing.body'),
    )
    return
  }

  loading.value = true
  feedback.value = null

  try {
    const created = await createBootstrapToken({
      token_kind: 'instance_admin',
      label: 'frontend-session',
      scopes: DEFAULT_BOOTSTRAP_SCOPES,
      workspace_id: null,
      bootstrap_secret: secret,
    })
    setApiBearerToken(created.token)
    refreshSessionDraft()
    bootstrapSecret.value = ''
    setFeedback(
      'success',
      t('flow.processing.auth.states.bootstrapSuccess.title'),
      t('flow.processing.auth.states.bootstrapSuccess.body'),
    )
    emit('updated')
  } catch (error) {
    const state = humanizeBootstrapError(error)
    setFeedback('warning', state.message, state.detail)
  } finally {
    loading.value = false
  }
}
</script>

<template>
  <article class="rr-panel rr-panel--accent auth-session-panel">
    <div class="auth-session-panel__hero">
      <div class="auth-session-panel__header">
        <div>
          <p class="rr-kicker">{{ t('flow.processing.auth.eyebrow') }}</p>
          <h3>{{ title ?? t('flow.processing.auth.title') }}</h3>
          <p class="rr-note auth-session-panel__lede">
            {{ description ?? t('flow.processing.auth.description') }}
          </p>
        </div>
        <StatusBadge :status="panelStatus.status" :label="panelStatus.label" emphasis="strong" />
      </div>

      <p class="auth-session-panel__summary">
        {{
          hasSessionToken
            ? t('flow.processing.auth.summary.ready')
            : t('flow.processing.auth.summary.pending')
        }}
      </p>

      <div class="auth-session-panel__steps">
        <article v-for="item in stepItems" :key="item.key" class="auth-step-card">
          <div class="auth-step-card__top">
            <div>
              <h4>{{ item.title }}</h4>
              <p>{{ item.body }}</p>
            </div>
            <StatusBadge :status="item.status" :label="item.label" />
          </div>
          <small>{{ item.hint }}</small>
        </article>
      </div>
    </div>

    <p v-if="contextNote" class="rr-note auth-session-panel__context">
      {{ contextNote }}
    </p>

    <div class="auth-session-panel__secondary">
      <button
        type="button"
        class="rr-button rr-button--secondary auth-session-panel__toggle"
        :aria-expanded="showAdvancedAccess ? 'true' : 'false'"
        @click="toggleAdvancedAccess"
      >
        {{
          showAdvancedAccess
            ? t('flow.processing.auth.secondary.hide')
            : t('flow.processing.auth.secondary.show')
        }}
      </button>
      <p class="rr-note">{{ t('flow.processing.auth.secondary.hint') }}</p>
    </div>

    <div v-if="showAdvancedAccess" class="auth-session-panel__grid">
      <section class="auth-session-panel__card auth-session-panel__card--manual">
        <div class="auth-session-panel__card-header">
          <div>
            <p class="rr-kicker">{{ t('flow.processing.auth.methods.manual.eyebrow') }}</p>
            <h4>{{ t('flow.processing.auth.methods.manual.title') }}</h4>
          </div>
          <StatusBadge
            :status="hasSessionToken ? 'Healthy' : 'Info'"
            :label="
              hasSessionToken
                ? t('flow.processing.auth.methods.manual.connected')
                : t('flow.processing.auth.methods.manual.ready')
            "
          />
        </div>

        <p class="rr-note">{{ t('flow.processing.auth.methods.manual.description') }}</p>

        <label class="rr-field">
          <span class="rr-field__label">{{ t('api.session.label') }}</span>
          <input
            v-model="sessionTokenDraft"
            class="rr-control"
            type="password"
            :placeholder="t('api.session.placeholder')"
            autocomplete="off"
          />
        </label>

        <div class="rr-action-row auth-session-panel__actions">
          <button
            type="button"
            class="rr-button"
            :disabled="loading || !hasDraftToken"
            @click="void saveSessionToken()"
          >
            {{ t('flow.processing.auth.methods.manual.action') }}
          </button>
          <button
            type="button"
            class="rr-button rr-button--secondary"
            :disabled="loading || !hasSessionToken"
            @click="void clearSessionToken()"
          >
            {{ t('api.session.actions.clear') }}
          </button>
        </div>
      </section>

      <section class="auth-session-panel__card auth-session-panel__card--bootstrap">
        <div class="auth-session-panel__card-header">
          <div>
            <p class="rr-kicker">{{ t('flow.processing.auth.methods.bootstrap.eyebrow') }}</p>
            <h4>{{ t('flow.processing.auth.methods.bootstrap.title') }}</h4>
          </div>
          <StatusBadge
            :status="hasBootstrapSecret ? 'Warning' : 'Info'"
            :label="
              hasBootstrapSecret
                ? t('flow.processing.auth.methods.bootstrap.ready')
                : t('flow.processing.auth.methods.bootstrap.optional')
            "
          />
        </div>

        <p class="rr-note">{{ t('flow.processing.auth.methods.bootstrap.description') }}</p>

        <label class="rr-field">
          <span class="rr-field__label">{{ t('api.session.bootstrap.label') }}</span>
          <input
            v-model="bootstrapSecret"
            class="rr-control"
            type="password"
            :placeholder="t('api.session.bootstrap.placeholder')"
            autocomplete="off"
          />
        </label>

        <div class="rr-action-row auth-session-panel__actions">
          <button
            type="button"
            class="rr-button"
            :disabled="loading || !hasBootstrapSecret"
            @click="void mintBootstrapSessionToken()"
          >
            {{
              loading
                ? t('api.session.bootstrap.actionBusy')
                : t('flow.processing.auth.methods.bootstrap.action')
            }}
          </button>
        </div>

        <p class="rr-note auth-session-panel__supporting-note">
          {{ t('flow.processing.auth.methods.bootstrap.hint') }}
        </p>
      </section>
    </div>

    <article class="auth-session-panel__active">
      <div class="auth-session-panel__active-header">
        <div>
          <p class="rr-kicker">{{ t('api.session.activeLabel') }}</p>
          <strong>{{ maskedSessionToken ?? t('api.session.activeNone') }}</strong>
        </div>
        <StatusBadge :status="panelStatus.status" :label="panelStatus.label" />
      </div>
      <p class="rr-note">
        {{
          hasSessionToken
            ? t('flow.processing.auth.activeDescription.ready')
            : t('flow.processing.auth.activeDescription.pending')
        }}
      </p>
    </article>

    <article
      v-if="feedback"
      class="rr-banner auth-session-panel__feedback"
      :data-tone="feedback.tone === 'success' ? 'success' : 'warning'"
    >
      <strong>{{ feedback.message }}</strong>
      <small v-if="feedback.detail">{{ feedback.detail }}</small>
    </article>
  </article>
</template>

<style scoped>
.auth-session-panel {
  display: grid;
  gap: var(--rr-space-5);
}

.auth-session-panel__hero {
  display: grid;
  gap: var(--rr-space-4);
  padding: clamp(var(--rr-space-4), 3vw, var(--rr-space-6));
  border-radius: var(--rr-radius-xl);
  background:
    linear-gradient(180deg, rgb(255 255 255 / 0.94), rgb(234 240 255 / 0.72)),
    var(--rr-color-bg-surface-strong);
  border: 1px solid rgb(29 78 216 / 0.12);
  box-shadow: var(--rr-shadow-sm);
}

.auth-session-panel__header {
  display: flex;
  justify-content: space-between;
  gap: var(--rr-space-4);
  align-items: flex-start;
}

.auth-session-panel__header h3,
.auth-session-panel__card-header h4,
.auth-session-panel__active strong,
.auth-step-card h4 {
  margin: 0;
}

.auth-session-panel__lede,
.auth-session-panel__summary,
.auth-step-card p,
.auth-step-card small,
.auth-session-panel__supporting-note,
.auth-session-panel__feedback small {
  margin: 0;
}

.auth-session-panel__summary {
  font-size: 1rem;
  line-height: 1.5;
  color: var(--rr-color-text-primary);
}

.auth-session-panel__secondary {
  display: grid;
  gap: var(--rr-space-2);
}

.auth-session-panel__toggle {
  justify-self: flex-start;
}

.auth-session-panel__secondary {
  display: grid;
  gap: var(--rr-space-2);
}

.auth-session-panel__toggle {
  justify-self: flex-start;
}

.auth-session-panel__steps {
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  gap: var(--rr-space-3);
}

.auth-step-card,
.auth-session-panel__card,
.auth-session-panel__active {
  display: grid;
  gap: var(--rr-space-3);
  padding: var(--rr-space-4);
  border-radius: var(--rr-radius-lg);
  background: rgb(255 255 255 / 0.8);
  border: 1px solid var(--rr-border-default);
}

.auth-step-card__top,
.auth-session-panel__card-header,
.auth-session-panel__active-header {
  display: flex;
  justify-content: space-between;
  gap: var(--rr-space-3);
  align-items: flex-start;
}

.auth-step-card h4,
.auth-session-panel__card-header h4 {
  font-size: 1rem;
}

.auth-step-card p {
  color: var(--rr-color-text-secondary);
}

.auth-step-card small,
.auth-session-panel__feedback small {
  color: var(--rr-color-text-muted);
  line-height: 1.45;
}

.auth-session-panel__context {
  margin-top: calc(-1 * var(--rr-space-2));
}

.auth-session-panel__grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: var(--rr-space-4);
}

.auth-session-panel__card {
  align-content: start;
}

.auth-session-panel__actions {
  flex-wrap: wrap;
}

.auth-session-panel__feedback {
  display: grid;
  gap: var(--rr-space-2);
}

@media (width <= 1024px) {
  .auth-session-panel__steps,
  .auth-session-panel__grid {
    grid-template-columns: 1fr;
  }
}

@media (width <= 760px) {
  .auth-session-panel {
    gap: var(--rr-space-4);
  }

  .auth-session-panel__hero {
    padding: var(--rr-space-4);
  }

  .auth-session-panel__header,
  .auth-step-card__top,
  .auth-session-panel__card-header,
  .auth-session-panel__active-header {
    flex-direction: column;
    align-items: flex-start;
  }

  .auth-session-panel__summary {
    font-size: 0.95rem;
  }
}
</style>
