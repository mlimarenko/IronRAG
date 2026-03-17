<script setup lang="ts">
import { computed, reactive, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import type {
  AdminSettingsResponse,
  UpdateAdminProviderProfilePayload,
} from 'src/models/ui/admin'

const props = defineProps<{
  settings: AdminSettingsResponse
  saving: boolean
  validating: boolean
}>()

const emit = defineEmits<{
  save: [payload: UpdateAdminProviderProfilePayload]
  validate: []
}>()

const i18n = useI18n()

const form = reactive<UpdateAdminProviderProfilePayload>({
  indexingProviderKind: 'openai',
  indexingModelName: '',
  embeddingProviderKind: 'openai',
  embeddingModelName: '',
  answerProviderKind: 'openai',
  answerModelName: '',
  visionProviderKind: 'openai',
  visionModelName: '',
})

watch(
  () => props.settings.providerProfile,
  (profile) => {
    form.indexingProviderKind = profile.indexingProviderKind
    form.indexingModelName = profile.indexingModelName
    form.embeddingProviderKind = profile.embeddingProviderKind
    form.embeddingModelName = profile.embeddingModelName
    form.answerProviderKind = profile.answerProviderKind
    form.answerModelName = profile.answerModelName
    form.visionProviderKind = profile.visionProviderKind
    form.visionModelName = profile.visionModelName
  },
  { immediate: true },
)

const providerCatalogByKind = computed(() =>
  new Map(props.settings.providerCatalog.map((provider) => [provider.providerKind, provider])),
)

function providerOptionsForCapability(capability: string): string[] {
  const configured = props.settings.providerCatalog
    .filter((provider) => provider.isConfigured && provider.supportedCapabilities.includes(capability))
    .map((provider) => provider.providerKind)

  if (configured.length > 0) {
    return configured
  }

  return props.settings.providerCatalog
    .filter((provider) => provider.supportedCapabilities.includes(capability))
    .map((provider) => provider.providerKind)
}

function defaultModelFor(providerKind: string, role: string): string {
  return providerCatalogByKind.value.get(providerKind)?.defaultModels[role] ?? ''
}

function modelOptionsFor(providerKind: string, role: string, currentModelName: string): string[] {
  const provider = providerCatalogByKind.value.get(providerKind)
  const options = [...(provider?.availableModels[role] ?? [])]
  const current = currentModelName.trim()
  if (current && !options.some((option) => option.toLowerCase() === current.toLowerCase())) {
    options.unshift(current)
  }
  return options
}

function ensureRoleSelection(
  providerKey: keyof UpdateAdminProviderProfilePayload,
  modelKey: keyof UpdateAdminProviderProfilePayload,
  capability: string,
  role: string,
): void {
  const supportedProviderKinds = providerOptionsForCapability(capability)
  if (supportedProviderKinds.length === 0) {
    return
  }

  if (!supportedProviderKinds.includes(form[providerKey])) {
    form[providerKey] = supportedProviderKinds[0]
    form[modelKey] = defaultModelFor(form[providerKey], role)
    return
  }

  if (!form[modelKey].trim()) {
    form[modelKey] = defaultModelFor(form[providerKey], role)
    return
  }

  const supportedModels = modelOptionsFor(form[providerKey], role, form[modelKey])
  if (
    supportedModels.length > 0 &&
    !supportedModels.some((model) => model.toLowerCase() === form[modelKey].trim().toLowerCase())
  ) {
    form[modelKey] = defaultModelFor(form[providerKey], role)
  }
}

function applyProviderChange(
  providerKey: keyof UpdateAdminProviderProfilePayload,
  modelKey: keyof UpdateAdminProviderProfilePayload,
  role: string,
): void {
  const fallbackModel = defaultModelFor(form[providerKey], role)
  if (fallbackModel) {
    form[modelKey] = fallbackModel
  }
}

const indexingProviderKinds = computed(() => providerOptionsForCapability('chat'))
const embeddingProviderKinds = computed(() => providerOptionsForCapability('embeddings'))
const answerProviderKinds = computed(() => providerOptionsForCapability('chat'))
const visionProviderKinds = computed(() => providerOptionsForCapability('vision'))

const indexingModelOptions = computed(() =>
  modelOptionsFor(form.indexingProviderKind, 'indexing', form.indexingModelName),
)
const embeddingModelOptions = computed(() =>
  modelOptionsFor(form.embeddingProviderKind, 'embedding', form.embeddingModelName),
)
const answerModelOptions = computed(() =>
  modelOptionsFor(form.answerProviderKind, 'answer', form.answerModelName),
)
const visionModelOptions = computed(() =>
  modelOptionsFor(form.visionProviderKind, 'vision', form.visionModelName),
)

watch(
  () => props.settings.providerProfile,
  () => {
    ensureRoleSelection('indexingProviderKind', 'indexingModelName', 'chat', 'indexing')
    ensureRoleSelection('embeddingProviderKind', 'embeddingModelName', 'embeddings', 'embedding')
    ensureRoleSelection('answerProviderKind', 'answerModelName', 'chat', 'answer')
    ensureRoleSelection('visionProviderKind', 'visionModelName', 'vision', 'vision')
  },
  { immediate: true },
)

const providerSections = computed(() => [
  {
    key: 'indexing',
    label: 'indexing',
    providerKey: 'indexingProviderKind' as const,
    modelKey: 'indexingModelName' as const,
    providerKinds: indexingProviderKinds.value,
    modelOptions: indexingModelOptions.value,
    role: 'indexing',
  },
  {
    key: 'embedding',
    label: 'embedding',
    providerKey: 'embeddingProviderKind' as const,
    modelKey: 'embeddingModelName' as const,
    providerKinds: embeddingProviderKinds.value,
    modelOptions: embeddingModelOptions.value,
    role: 'embedding',
  },
  {
    key: 'answer',
    label: 'answer',
    providerKey: 'answerProviderKind' as const,
    modelKey: 'answerModelName' as const,
    providerKinds: answerProviderKinds.value,
    modelOptions: answerModelOptions.value,
    role: 'answer',
  },
  {
    key: 'vision',
    label: 'vision',
    providerKey: 'visionProviderKind' as const,
    modelKey: 'visionModelName' as const,
    providerKinds: visionProviderKinds.value,
    modelOptions: visionModelOptions.value,
    role: 'vision',
  },
])

const validationStatusClass = computed(() => {
  const status = props.settings.providerValidation.status ?? props.settings.providerProfile.lastValidationStatus
  if (status === 'passed') {
    return 'is-success'
  }
  if (status === 'failed') {
    return 'is-danger'
  }
  return 'is-muted'
})

const settingsItemMap = computed(
  () => new Map(props.settings.items.map((item) => [item.id, item])),
)

const libraryFacts = computed(() => [
  {
    key: 'upload',
    title: 'uploadLimit',
    value: settingsItemMap.value.get('upload_limit')?.value ?? '—',
  },
  {
    key: 'locale',
    title: 'defaultLocale',
    value: settingsItemMap.value.get('default_locale')?.value ?? '—',
  },
  {
    key: 'session',
    title: 'session',
    value: settingsItemMap.value.get('session_ttl')?.value ?? '—',
  },
])

const configuredProviders = computed(() =>
  props.settings.providerCatalog
    .filter((provider) => provider.isConfigured)
    .map((provider) => ({
      providerKind: provider.providerKind,
      roles: [
        { key: 'indexing', models: provider.availableModels.indexing ?? [] },
        { key: 'embedding', models: provider.availableModels.embedding ?? [] },
        { key: 'answer', models: provider.availableModels.answer ?? [] },
        { key: 'vision', models: provider.availableModels.vision ?? [] },
      ].filter((role) => role.models.length > 0),
    })),
)

const lastCheckedAt = computed(() => {
  const raw = props.settings.providerValidation.checkedAt ?? props.settings.providerProfile.lastValidatedAt
  if (!raw) {
    return null
  }

  try {
    return new Intl.DateTimeFormat(undefined, {
      year: 'numeric',
      month: '2-digit',
      day: '2-digit',
      hour: '2-digit',
      minute: '2-digit',
    }).format(new Date(raw))
  } catch {
    return raw
  }
})

const pricingCoverageClass = computed(() => {
  if (props.settings.pricingCoverage.status === 'covered') {
    return 'is-configured'
  }
  if (props.settings.pricingCoverage.status === 'missing') {
    return 'is-danger'
  }
  return 'is-warning'
})

const pricingCoverageWarnings = computed(() =>
  props.settings.pricingCoverage.warnings.map((warning) => ({
    ...warning,
    capabilityLabel: optionLabel('admin.pricing.capabilityLabels', warning.capability),
    billingUnitLabel: optionLabel('admin.pricing.billingUnitLabels', warning.billingUnit),
  })),
)

function optionLabel(prefix: string, value: string): string {
  const key = `${prefix}.${value}`
  return i18n.te(key) ? i18n.t(key) : value
}

function submit(): void {
  ensureRoleSelection('indexingProviderKind', 'indexingModelName', 'chat', 'indexing')
  ensureRoleSelection('embeddingProviderKind', 'embeddingModelName', 'embeddings', 'embedding')
  ensureRoleSelection('answerProviderKind', 'answerModelName', 'chat', 'answer')
  ensureRoleSelection('visionProviderKind', 'visionModelName', 'vision', 'vision')

  emit('save', {
    indexingProviderKind: form.indexingProviderKind,
    indexingModelName: form.indexingModelName.trim(),
    embeddingProviderKind: form.embeddingProviderKind,
    embeddingModelName: form.embeddingModelName.trim(),
    answerProviderKind: form.answerProviderKind,
    answerModelName: form.answerModelName.trim(),
    visionProviderKind: form.visionProviderKind,
    visionModelName: form.visionModelName.trim(),
  })
}
</script>

<template>
  <section class="rr-page-card rr-admin-settings">
    <header class="rr-admin-settings__header">
      <div>
        <h3>{{ $t('admin.settings.providerTitle') }}</h3>
        <p>{{ $t('admin.settings.providerSubtitle', { library: settings.providerProfile.libraryName }) }}</p>
      </div>
      <span
        class="rr-status-pill"
        :class="validationStatusClass"
      >
        {{
          settings.providerValidation.status
            ? $t(`admin.settings.validation.${settings.providerValidation.status}`)
            : $t('admin.settings.validation.pending')
        }}
      </span>
    </header>

    <div class="rr-admin-settings__layout">
      <section class="rr-admin-settings__stack-card">
        <div class="rr-admin-settings__section-head">
          <div>
            <h4>{{ $t('admin.settings.activeStackTitle') }}</h4>
            <p>{{ $t('admin.settings.activeStackSubtitle') }}</p>
          </div>
          <div class="rr-admin-settings__actions">
            <button
              class="rr-button rr-button--ghost"
              type="button"
              :disabled="!settings.liveValidationEnabled || validating"
              @click="emit('validate')"
            >
              {{ validating ? $t('admin.settings.validating') : $t('admin.settings.validate') }}
            </button>
            <button
              class="rr-button"
              type="button"
              :disabled="saving"
              @click="submit"
            >
              {{ saving ? $t('admin.settings.saving') : $t('admin.settings.save') }}
            </button>
          </div>
        </div>

        <div
          v-if="settings.providerValidation.error"
          class="rr-admin-settings__warning"
        >
          {{ settings.providerValidation.error }}
        </div>

        <div
          v-if="pricingCoverageWarnings.length > 0"
          class="rr-admin-settings__warning"
        >
          <strong>{{ $t('admin.settings.pricingCoverage.warningTitle') }}</strong>
          <ul class="rr-admin-settings__policy-list rr-admin-settings__policy-list--compact">
            <li
              v-for="warning in pricingCoverageWarnings"
              :key="`${warning.providerKind}:${warning.modelName}:${warning.capability}:${warning.billingUnit}`"
            >
              <strong>{{ warning.providerKind }} / {{ warning.modelName }}</strong>
              <span>
                {{ warning.capabilityLabel }} · {{ warning.billingUnitLabel }} · {{ warning.message }}
              </span>
            </li>
          </ul>
        </div>

        <div class="rr-admin-settings__provider-grid">
          <article
            v-for="section in providerSections"
            :key="section.key"
            class="rr-admin-settings__provider-field"
          >
            <strong class="rr-admin-settings__provider-title">
              {{ $t(`admin.settings.roles.${section.label}`) }}
            </strong>
            <div class="rr-admin-settings__control-row">
              <select
                v-model="form[section.providerKey]"
                @change="applyProviderChange(section.providerKey, section.modelKey, section.role)"
              >
                <option
                  v-for="kind in section.providerKinds"
                  :key="`${section.key}-${kind}`"
                  :value="kind"
                >
                  {{ kind }}
                </option>
              </select>
              <select v-model="form[section.modelKey]">
                <option
                  v-for="model in section.modelOptions"
                  :key="`${section.key}-${model}`"
                  :value="model"
                >
                  {{ model }}
                </option>
              </select>
            </div>
          </article>
        </div>

        <div
          v-if="settings.providerValidation.checks.length > 0"
          class="rr-admin-settings__checks"
        >
          <div class="rr-admin-settings__checks-header">
            <h5>{{ $t('admin.settings.checksTitle') }}</h5>
            <span v-if="lastCheckedAt">{{ $t('admin.settings.lastChecked', { value: lastCheckedAt }) }}</span>
          </div>
          <table class="rr-admin-table">
            <thead>
              <tr>
                <th>{{ $t('admin.settings.checkHeaders.capability') }}</th>
                <th>{{ $t('admin.settings.checkHeaders.provider') }}</th>
                <th>{{ $t('admin.settings.checkHeaders.model') }}</th>
                <th>{{ $t('admin.settings.checkHeaders.status') }}</th>
              </tr>
            </thead>
            <tbody>
              <tr
                v-for="check in settings.providerValidation.checks"
                :key="`${check.capability}:${check.modelName}`"
              >
                <td>{{ check.capability }}</td>
                <td>{{ check.providerKind }}</td>
                <td>{{ check.modelName }}</td>
                <td>{{ check.status }}</td>
              </tr>
            </tbody>
          </table>
        </div>
      </section>

      <aside class="rr-admin-settings__side-column">
        <section class="rr-admin-settings__policy-card">
          <div class="rr-admin-settings__section-head">
            <div>
              <h4>{{ $t('admin.settings.pricingCoverage.title') }}</h4>
              <p>{{ $t('admin.settings.pricingCoverage.subtitle') }}</p>
            </div>
            <span
              class="rr-admin-settings__provider-state"
              :class="pricingCoverageClass"
            >
              {{ $t(`admin.settings.pricingCoverage.status.${settings.pricingCoverage.status}`) }}
            </span>
          </div>

          <ul class="rr-admin-settings__policy-list">
            <li>
              <strong>{{ $t('admin.settings.pricingCoverage.covered') }}</strong>
              <span>{{ settings.pricingCoverage.coveredTargets }}</span>
            </li>
            <li>
              <strong>{{ $t('admin.settings.pricingCoverage.missing') }}</strong>
              <span>{{ settings.pricingCoverage.missingTargets }}</span>
            </li>
          </ul>
        </section>

        <section class="rr-admin-settings__policy-card">
          <div class="rr-admin-settings__section-head">
            <div>
              <h4>{{ $t('admin.settings.libraryTitle') }}</h4>
              <p>{{ $t('admin.settings.librarySubtitle') }}</p>
            </div>
          </div>

          <ul class="rr-admin-settings__policy-list">
            <li
              v-for="item in libraryFacts"
              :key="item.key"
            >
              <strong>{{ $t(`admin.settings.policy.${item.title}`) }}</strong>
              <span>{{ item.value }}</span>
            </li>
          </ul>
        </section>

        <section class="rr-admin-settings__policy-card">
          <div class="rr-admin-settings__section-head">
            <div>
              <h4>{{ $t('admin.settings.availableProvidersTitle') }}</h4>
              <p>{{ $t('admin.settings.availableProvidersSubtitle') }}</p>
            </div>
          </div>

          <div class="rr-admin-settings__catalog">
            <article
              v-for="provider in configuredProviders"
              :key="provider.providerKind"
              class="rr-admin-settings__catalog-card"
            >
              <header>
                <strong>{{ provider.providerKind }}</strong>
                <small class="rr-admin-settings__provider-state is-configured">
                  {{ $t('admin.settings.providerState.configured') }}
                </small>
              </header>

              <div class="rr-admin-settings__catalog-grid">
                <div
                  v-for="role in provider.roles"
                  :key="`${provider.providerKind}-${role.key}`"
                >
                  <span>{{ $t(`admin.settings.roles.${role.key}`) }}</span>
                  <code>{{ role.models.join(', ') }}</code>
                </div>
              </div>
            </article>
          </div>
        </section>
      </aside>
    </div>
  </section>
</template>
