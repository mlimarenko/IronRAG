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
const feedback = ref<{ tone: 'success' | 'warning'; message: string } | null>(null)

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

function refreshSessionDraft() {
  sessionToken.value = getApiBearerToken()
  sessionTokenDraft.value = sessionToken.value
}

function saveSessionToken() {
  setApiBearerToken(sessionTokenDraft.value)
  refreshSessionDraft()
  feedback.value = {
    tone: hasSessionToken.value ? 'success' : 'warning',
    message: hasSessionToken.value
      ? t('api.session.feedback.saved')
      : t('api.session.feedback.cleared'),
  }
  emit('updated')
}

function clearSessionToken() {
  clearApiBearerToken()
  refreshSessionDraft()
  feedback.value = {
    tone: 'warning',
    message: t('api.session.feedback.cleared'),
  }
  emit('updated')
}

async function mintBootstrapSessionToken() {
  const secret = bootstrapSecret.value.trim()
  if (!secret) {
    feedback.value = {
      tone: 'warning',
      message: t('api.session.bootstrap.missingSecret'),
    }
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
    feedback.value = {
      tone: 'success',
      message: t('api.session.bootstrap.success'),
    }
    emit('updated')
  } catch (error) {
    feedback.value = {
      tone: 'warning',
      message: isBootstrapNotConfiguredApiError(error)
        ? t('api.session.bootstrap.notConfigured')
        : isUnauthorizedApiError(error)
          ? t('api.session.bootstrap.rejected')
          : error instanceof Error
            ? error.message
            : t('api.page.errors.unknown'),
    }
  } finally {
    loading.value = false
  }
}
</script>

<template>
  <article class="rr-panel auth-session-panel">
    <div class="auth-session-panel__header">
      <div>
        <p class="rr-kicker">{{ t('api.session.eyebrow') }}</p>
        <h3>{{ title ?? t('api.session.title') }}</h3>
        <p class="rr-note">{{ description ?? t('api.session.description') }}</p>
      </div>
      <StatusBadge :status="panelStatus.status" :label="panelStatus.label" />
    </div>

    <p v-if="contextNote" class="rr-note auth-session-panel__note">
      {{ contextNote }}
    </p>

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
        :disabled="loading || !sessionTokenDraft.trim()"
        @click="void saveSessionToken()"
      >
        {{ t('api.session.actions.save') }}
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

    <div class="auth-session-panel__bootstrap">
      <label class="rr-field">
        <span class="rr-field__label">{{ t('api.session.bootstrap.label') }}</span>
        <input
          v-model="bootstrapSecret"
          class="rr-control"
          type="password"
          :placeholder="t('api.session.bootstrap.placeholder')"
          autocomplete="off"
        >
      </label>
      <div class="rr-action-row">
        <button
          type="button"
          class="rr-button"
          :disabled="loading || !bootstrapSecret.trim()"
          @click="void mintBootstrapSessionToken()"
        >
          {{
            loading
              ? t('api.session.bootstrap.actionBusy')
              : t('api.session.bootstrap.action')
          }}
        </button>
      </div>
      <p class="rr-note">
        {{ t('api.session.bootstrap.hint') }}
      </p>
    </div>

    <article class="auth-session-panel__active">
      <div>
        <p class="rr-kicker">{{ t('api.session.activeLabel') }}</p>
        <strong>{{ maskedSessionToken ?? t('api.session.activeNone') }}</strong>
      </div>
      <p class="rr-note">
        {{
          hasSessionToken
            ? t('api.session.activeDescription')
            : t('api.session.missingDescription')
        }}
      </p>
    </article>

    <p v-if="feedback" class="rr-banner" :data-tone="feedback.tone === 'success' ? 'success' : 'warning'">
      {{ feedback.message }}
    </p>
  </article>
</template>

<style scoped>
.auth-session-panel {
  display: grid;
  gap: var(--rr-space-4);
}

.auth-session-panel__header {
  display: flex;
  justify-content: space-between;
  gap: var(--rr-space-4);
  align-items: flex-start;
}

.auth-session-panel__header h3,
.auth-session-panel__active strong {
  margin: 0;
}

.auth-session-panel__bootstrap,
.auth-session-panel__active {
  display: grid;
  gap: var(--rr-space-3);
  padding: var(--rr-space-4);
  border-radius: var(--rr-radius-lg);
  background: rgba(15, 23, 42, 0.04);
  border: 1px solid rgba(15, 23, 42, 0.08);
}

.auth-session-panel__note {
  margin-top: calc(-1 * var(--rr-space-2));
}

@media (width <= 760px) {
  .auth-session-panel__header {
    flex-direction: column;
    align-items: flex-start;
  }
}
</style>
