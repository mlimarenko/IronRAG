<script setup lang="ts">
import { computed, reactive, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import type {
  BootstrapAiSetupDescriptor,
  BootstrapBindingPurpose,
  BootstrapSetupAiPayload,
} from 'src/models/ui/auth'
import {
  PURPOSE_ORDER,
  buildBootstrapSetupAiPayload,
  createEmptyBindingDraft,
  defaultBindingInput,
  envConfiguredProviders as resolveEnvConfiguredProviders,
  isAiSetupReady,
  missingCredentialProviders as resolveMissingCredentialProviders,
  modelsForPurpose as resolveModelsForPurpose,
  providersForPurpose as resolveProvidersForPurpose,
  syncBindingInput as resolveSyncBindingInput,
  unavailablePurposes as resolveUnavailablePurposes,
} from './bootstrapSetupForm'

const props = defineProps<{
  aiSetup: BootstrapAiSetupDescriptor | null
  loading: boolean
  error: string | null
}>()

const emit = defineEmits<{
  submit: [
    payload: {
      login: string
      displayName: string
      password: string
      aiSetup: BootstrapSetupAiPayload | null
    },
  ]
}>()

const { t } = useI18n()
const form = reactive({
  login: '',
  displayName: '',
  password: '',
})
const credentialDraft = reactive<Record<string, string>>({})
const bindingDraft = reactive(createEmptyBindingDraft())

const displayNamePlaceholder = computed(
  () => form.login.trim() || t('auth.setup.displayNamePlaceholder'),
)

function providersForPurpose(purpose: BootstrapBindingPurpose) {
  return resolveProvidersForPurpose(props.aiSetup, purpose)
}

function modelsForPurpose(purpose: BootstrapBindingPurpose, providerKind: string) {
  return resolveModelsForPurpose(props.aiSetup, purpose, providerKind)
}

function syncBindingInput(purpose: BootstrapBindingPurpose) {
  bindingDraft[purpose] = resolveSyncBindingInput(props.aiSetup, purpose, bindingDraft[purpose])
}

watch(
  () => props.aiSetup,
  () => {
    if (!props.aiSetup) {
      return
    }
    for (const purpose of PURPOSE_ORDER) {
      bindingDraft[purpose] = defaultBindingInput(props.aiSetup, purpose)
    }
  },
  { immediate: true },
)

const envConfiguredProviders = computed(() =>
  resolveEnvConfiguredProviders(props.aiSetup, bindingDraft),
)

const missingCredentialProviders = computed(() =>
  resolveMissingCredentialProviders(props.aiSetup, bindingDraft),
)

const unavailablePurposes = computed(() => resolveUnavailablePurposes(props.aiSetup))

const aiSetupReady = computed(() => {
  return isAiSetupReady(props.aiSetup, bindingDraft, credentialDraft)
})

const canSubmit = computed(
  () => form.login.trim().length > 0 && form.password.trim().length >= 8 && aiSetupReady.value,
)

function submit() {
  emit('submit', {
    login: form.login.trim().toLowerCase(),
    displayName: form.displayName.trim(),
    password: form.password,
    aiSetup: buildBootstrapSetupAiPayload(props.aiSetup, bindingDraft, credentialDraft),
  })
}
</script>

<template>
  <div class="rr-auth-card rr-auth-card--bootstrap">
    <h2>{{ t('auth.setup.title') }}</h2>
    <p>{{ t('auth.setup.subtitle') }}</p>

    <form class="rr-form rr-bootstrap-form" @submit.prevent="submit">
      <div class="rr-bootstrap-form__grid">
        <section class="rr-bootstrap-section">
          <div class="rr-bootstrap-section__header">
            <h3>{{ t('auth.setup.sections.admin') }}</h3>
            <p>{{ t('auth.setup.hint') }}</p>
          </div>

          <div class="rr-field">
            <label for="setup-login">{{ t('auth.login') }}</label>
            <input
              id="setup-login"
              v-model="form.login"
              type="text"
              placeholder="admin"
              autocomplete="username"
            />
          </div>

          <div class="rr-field">
            <label for="setup-display-name">{{ t('auth.setup.displayName') }}</label>
            <input
              id="setup-display-name"
              v-model="form.displayName"
              type="text"
              :placeholder="displayNamePlaceholder"
              autocomplete="name"
            />
          </div>

          <div class="rr-field">
            <label for="setup-password">{{ t('auth.password') }}</label>
            <input
              id="setup-password"
              v-model="form.password"
              type="password"
              placeholder="••••••••"
              autocomplete="new-password"
            />
          </div>
        </section>

        <section v-if="props.aiSetup" class="rr-bootstrap-section rr-bootstrap-section--ai">
          <div class="rr-bootstrap-section__header">
            <h3>{{ t('auth.setup.ai.title') }}</h3>
            <p>{{ t('auth.setup.ai.subtitle') }}</p>
          </div>

          <div v-if="unavailablePurposes.length" class="rr-error-card rr-error-card--inline">
            {{ t('auth.setup.ai.validation.catalogUnavailable') }}
          </div>

          <div class="rr-bootstrap-purpose-list">
            <article
              v-for="purpose in PURPOSE_ORDER"
              :key="purpose"
              class="rr-bootstrap-purpose-card"
            >
              <div class="rr-bootstrap-purpose-card__header">
                <h4>{{ t(`auth.setup.ai.bindings.${purpose}.label`) }}</h4>
                <p>{{ t(`auth.setup.ai.bindings.${purpose}.description`) }}</p>
              </div>

              <div class="rr-bootstrap-purpose-card__grid">
                <div class="rr-field">
                  <label :for="`bootstrap-provider-${purpose}`">
                    {{ t('auth.setup.ai.provider') }}
                  </label>
                  <select
                    :id="`bootstrap-provider-${purpose}`"
                    v-model="bindingDraft[purpose].providerKind"
                    :disabled="props.loading || providersForPurpose(purpose).length === 0"
                    @change="syncBindingInput(purpose)"
                  >
                    <option v-if="providersForPurpose(purpose).length === 0" value="">
                      {{ t('auth.setup.ai.bindings.unavailable') }}
                    </option>
                    <option
                      v-for="provider in providersForPurpose(purpose)"
                      :key="provider.providerKind"
                      :value="provider.providerKind"
                    >
                      {{ provider.displayName }}
                    </option>
                  </select>
                </div>

                <div class="rr-field">
                  <label :for="`bootstrap-model-${purpose}`">
                    {{ t('auth.setup.ai.model') }}
                  </label>
                  <select
                    :id="`bootstrap-model-${purpose}`"
                    v-model="bindingDraft[purpose].modelCatalogId"
                    :disabled="
                      props.loading ||
                      modelsForPurpose(purpose, bindingDraft[purpose].providerKind).length === 0
                    "
                  >
                    <option
                      v-if="
                        modelsForPurpose(purpose, bindingDraft[purpose].providerKind).length === 0
                      "
                      value=""
                    >
                      {{ t('auth.setup.ai.bindings.unavailable') }}
                    </option>
                    <option
                      v-for="model in modelsForPurpose(purpose, bindingDraft[purpose].providerKind)"
                      :key="model.id"
                      :value="model.id"
                    >
                      {{ model.modelName }}
                    </option>
                  </select>
                </div>
              </div>
            </article>
          </div>

          <div class="rr-bootstrap-credentials">
            <div class="rr-bootstrap-credentials__header">
              <h4>{{ t('auth.setup.ai.credentialsTitle') }}</h4>
              <p>{{ t('auth.setup.ai.credentialsSubtitle') }}</p>
            </div>

            <div v-if="envConfiguredProviders.length" class="rr-bootstrap-credentials__notice-list">
              <p
                v-for="provider in envConfiguredProviders"
                :key="provider.providerKind"
                class="rr-bootstrap-credentials__notice"
              >
                {{ `${provider.displayName}: ${t('auth.setup.ai.providers.envConfigured')}` }}
              </p>
            </div>

            <div v-if="missingCredentialProviders.length" class="rr-bootstrap-credentials__grid">
              <div
                v-for="provider in missingCredentialProviders"
                :key="provider.providerKind"
                class="rr-field"
              >
                <label :for="`bootstrap-api-key-${provider.providerKind}`">
                  {{ `${provider.displayName} ${t('auth.setup.ai.apiKey')}` }}
                </label>
                <input
                  :id="`bootstrap-api-key-${provider.providerKind}`"
                  v-model="credentialDraft[provider.providerKind]"
                  type="password"
                  :placeholder="t('auth.setup.ai.apiKeyPlaceholder')"
                  autocomplete="off"
                />
              </div>
            </div>
          </div>
        </section>
      </div>

      <button
        class="rr-button rr-button--primary"
        type="submit"
        :disabled="props.loading || !canSubmit"
      >
        {{ t('auth.setup.submit') }}
      </button>

      <p v-if="props.error" class="rr-error-card">
        {{ props.error }}
      </p>
    </form>
  </div>
</template>
