<script setup lang="ts">
import { computed, reactive, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import type {
  AdminPricingCatalogEntry,
  AdminSettingsResponse,
  AdminUpsertPricingEntryPayload,
} from 'src/models/ui/admin'

const props = defineProps<{
  settings: AdminSettingsResponse
  saving: boolean
}>()

const emit = defineEmits<{
  create: [payload: AdminUpsertPricingEntryPayload]
  update: [pricingId: string, payload: AdminUpsertPricingEntryPayload]
  deactivate: [pricingId: string]
}>()

const i18n = useI18n()

const editingId = ref<string | null>(null)
const submitAttempted = ref(false)
const form = reactive({
  workspaceId: null as string | null,
  providerKind: 'openai',
  modelName: '',
  capability: 'indexing',
  billingUnit: 'per_1m_tokens',
  inputPrice: '',
  outputPrice: '',
  currency: 'USD',
  note: '',
  effectiveFrom: '',
})

const capabilityOptions = ['indexing', 'embedding', 'answer', 'vision', 'graph_extract']
const billingUnitOptions = [
  'per_1m_input_tokens',
  'per_1m_output_tokens',
  'per_1m_tokens',
  'fixed_per_call',
]

const providerOptions = computed(() => {
  const configured = props.settings.providerCatalog
    .filter((provider) => provider.isConfigured)
    .map((provider) => provider.providerKind)
  return configured.length > 0 ? configured : props.settings.supportedProviderKinds
})

const providerCatalogByKind = computed(
  () => new Map(props.settings.providerCatalog.map((provider) => [provider.providerKind, provider])),
)

const coverageToneClass = computed(() => {
  if (props.settings.pricingCoverage.status === 'covered') {
    return 'is-configured'
  }
  if (props.settings.pricingCoverage.status === 'missing') {
    return 'is-danger'
  }
  return 'is-warning'
})

const roleForCapability = computed(() => {
  switch (form.capability) {
    case 'embedding':
      return 'embedding'
    case 'answer':
      return 'answer'
    case 'vision':
      return 'vision'
    default:
      return 'indexing'
  }
})

const modelOptions = computed(() => {
  const provider = providerCatalogByKind.value.get(form.providerKind)
  const options = [...(provider?.availableModels[roleForCapability.value] ?? [])]
  const current = form.modelName.trim()
  if (current && !options.some((option) => option.toLowerCase() === current.toLowerCase())) {
    options.unshift(current)
  }
  return options
})

const selectedProviderConfigured = computed(
  () => providerCatalogByKind.value.get(form.providerKind)?.isConfigured ?? false,
)

const pricingWarnings = computed(() =>
  props.settings.pricingCoverage.warnings.map((warning) => ({
    ...warning,
    capabilityLabel: optionLabel('admin.pricing.capabilityLabels', warning.capability),
    billingUnitLabel: optionLabel('admin.pricing.billingUnitLabels', warning.billingUnit),
  })),
)

const sortedRows = computed(() =>
  [...props.settings.pricingCatalog].sort((left, right) => {
    if (left.status !== right.status) {
      return left.status === 'active' ? -1 : 1
    }
    return right.effectiveFrom.localeCompare(left.effectiveFrom)
  }),
)

const validationErrors = computed(() => {
  const errors: string[] = []
  if (!form.providerKind.trim()) {
    errors.push(i18n.t('admin.pricing.validation.providerRequired'))
  }
  if (!form.modelName.trim()) {
    errors.push(i18n.t('admin.pricing.validation.modelRequired'))
  }
  if (!form.effectiveFrom.trim()) {
    errors.push(i18n.t('admin.pricing.validation.effectiveFromRequired'))
  }

  const parsedInput = parseOptionalNumber(form.inputPrice)
  const parsedOutput = parseOptionalNumber(form.outputPrice)
  if (!isEmpty(form.inputPrice) && parsedInput === null) {
    errors.push(i18n.t('admin.pricing.validation.inputPriceInvalid'))
  }
  if (!isEmpty(form.outputPrice) && parsedOutput === null) {
    errors.push(i18n.t('admin.pricing.validation.outputPriceInvalid'))
  }
  if (parsedInput !== null && parsedInput < 0) {
    errors.push(i18n.t('admin.pricing.validation.inputPriceNegative'))
  }
  if (parsedOutput !== null && parsedOutput < 0) {
    errors.push(i18n.t('admin.pricing.validation.outputPriceNegative'))
  }
  if (parsedInput === null && parsedOutput === null) {
    errors.push(i18n.t('admin.pricing.validation.onePriceRequired'))
  }
  if (!form.currency.trim()) {
    errors.push(i18n.t('admin.pricing.validation.currencyRequired'))
  }

  return errors
})

const canSubmit = computed(() => validationErrors.value.length === 0)
const submitLabel = computed(() => {
  if (props.saving) {
    return i18n.t('admin.pricing.saving')
  }
  return editingId.value ? i18n.t('admin.pricing.update') : i18n.t('admin.pricing.create')
})

watch(
  providerOptions,
  (options) => {
    if (options.length === 0) {
      return
    }
    if (!options.includes(form.providerKind)) {
      form.providerKind = options[0]
    }
  },
  { immediate: true },
)

watch(
  () => [form.providerKind, form.capability] as const,
  ([providerKind]) => {
    if (form.modelName.trim()) {
      return
    }
    const provider = providerCatalogByKind.value.get(providerKind)
    if (!provider) {
      return
    }
    const defaultModel = provider.defaultModels[roleForCapability.value]
    if (defaultModel) {
      form.modelName = defaultModel
    }
  },
  { immediate: true },
)

function optionLabel(prefix: string, value: string): string {
  const key = `${prefix}.${value}`
  return i18n.te(key) ? i18n.t(key) : value
}

function isEmpty(value: string): boolean {
  return value.trim().length === 0
}

function parseOptionalNumber(value: string): number | null {
  const normalized = value.trim()
  if (!normalized) {
    return null
  }
  const parsed = Number(normalized)
  return Number.isFinite(parsed) ? parsed : null
}

function toDatetimeLocal(value: string): string {
  const parsed = new Date(value)
  if (Number.isNaN(parsed.getTime())) {
    return ''
  }
  const offset = parsed.getTimezoneOffset()
  const local = new Date(parsed.getTime() - offset * 60_000)
  return local.toISOString().slice(0, 16)
}

function fromDatetimeLocal(value: string): string {
  return value ? new Date(value).toISOString() : new Date().toISOString()
}

function formatDateTime(value: string | null): string {
  if (!value) {
    return '—'
  }
  const parsed = new Date(value)
  if (Number.isNaN(parsed.getTime())) {
    return value
  }
  return parsed.toLocaleString()
}

function formatScope(row: AdminPricingCatalogEntry): string {
  return row.workspaceId
    ? i18n.t('admin.pricing.scope.workspace', { id: row.workspaceId.slice(0, 8) })
    : i18n.t('admin.pricing.scope.global')
}

function formatPrice(value: string | null, currency: string): string {
  if (value === null) {
    return '—'
  }
  const parsed = Number(value)
  if (!Number.isFinite(parsed)) {
    return `${value} ${currency}`
  }
  try {
    return new Intl.NumberFormat(undefined, {
      style: 'currency',
      currency,
      maximumFractionDigits: 6,
    }).format(parsed)
  } catch {
    return `${parsed.toFixed(6)} ${currency}`
  }
}

function resetForm(): void {
  editingId.value = null
  submitAttempted.value = false
  form.workspaceId = null
  form.providerKind = providerOptions.value[0] || 'openai'
  form.modelName = ''
  form.capability = 'indexing'
  form.billingUnit = 'per_1m_tokens'
  form.inputPrice = ''
  form.outputPrice = ''
  form.currency = 'USD'
  form.note = ''
  form.effectiveFrom = toDatetimeLocal(new Date().toISOString())
}

function startEdit(row: AdminPricingCatalogEntry): void {
  editingId.value = row.id
  submitAttempted.value = false
  form.workspaceId = row.workspaceId
  form.providerKind = row.providerKind
  form.modelName = row.modelName
  form.capability = row.capability
  form.billingUnit = row.billingUnit
  form.inputPrice = row.inputPrice ?? ''
  form.outputPrice = row.outputPrice ?? ''
  form.currency = row.currency
  form.note = row.note ?? ''
  form.effectiveFrom = toDatetimeLocal(row.effectiveFrom)
}

function buildPayload(): AdminUpsertPricingEntryPayload {
  return {
    workspaceId: form.workspaceId,
    providerKind: form.providerKind,
    modelName: form.modelName.trim(),
    capability: form.capability,
    billingUnit: form.billingUnit,
    inputPrice: parseOptionalNumber(form.inputPrice),
    outputPrice: parseOptionalNumber(form.outputPrice),
    currency: form.currency.trim().toUpperCase(),
    note: form.note.trim() || null,
    effectiveFrom: fromDatetimeLocal(form.effectiveFrom),
  }
}

function submit(): void {
  submitAttempted.value = true
  if (!canSubmit.value) {
    return
  }
  if (editingId.value) {
    emit('update', editingId.value, buildPayload())
    return
  }
  emit('create', buildPayload())
}

resetForm()
</script>

<template>
  <section class="rr-page-card rr-admin-settings">
    <header class="rr-admin-settings__header">
      <div>
        <h3>{{ $t('admin.pricing.title') }}</h3>
        <p>{{ $t('admin.pricing.subtitle') }}</p>
      </div>
      <span
        class="rr-admin-settings__provider-state"
        :class="coverageToneClass"
      >
        {{ $t(`admin.settings.pricingCoverage.status.${settings.pricingCoverage.status}`) }}
      </span>
    </header>

    <div class="rr-admin-settings__layout">
      <section class="rr-admin-settings__stack-card">
        <div class="rr-admin-settings__section-head">
          <div>
            <h4>{{ editingId ? $t('admin.pricing.editTitle') : $t('admin.pricing.createTitle') }}</h4>
            <p>{{ $t('admin.pricing.formSubtitle') }}</p>
          </div>
          <div class="rr-admin-settings__actions">
            <button
              v-if="editingId"
              class="rr-button rr-button--ghost"
              type="button"
              :disabled="saving"
              @click="resetForm"
            >
              {{ $t('admin.pricing.cancelEdit') }}
            </button>
            <button
              class="rr-button"
              type="button"
              :disabled="saving"
              @click="submit"
            >
              {{ submitLabel }}
            </button>
          </div>
        </div>

        <div
          v-if="submitAttempted && validationErrors.length > 0"
          class="rr-admin-settings__warning"
        >
          <strong>{{ $t('admin.pricing.validation.title') }}</strong>
          <ul class="rr-admin-settings__policy-list rr-admin-settings__policy-list--compact">
            <li
              v-for="errorMessage in validationErrors"
              :key="errorMessage"
            >
              <span>{{ errorMessage }}</span>
            </li>
          </ul>
        </div>

        <div class="rr-admin-settings__provider-grid">
          <article class="rr-admin-settings__provider-field">
            <strong class="rr-admin-settings__provider-title">{{ $t('admin.pricing.fields.provider') }}</strong>
            <select v-model="form.providerKind">
              <option
                v-for="providerKind in providerOptions"
                :key="providerKind"
                :value="providerKind"
              >
                {{ providerKind }}
              </option>
            </select>
            <span class="rr-admin-settings__field-hint">
              {{
                selectedProviderConfigured
                  ? $t('admin.pricing.providerConfigured')
                  : $t('admin.pricing.providerUnconfigured')
              }}
            </span>
          </article>

          <article class="rr-admin-settings__provider-field">
            <strong class="rr-admin-settings__provider-title">{{ $t('admin.pricing.fields.model') }}</strong>
            <input
              v-model="form.modelName"
              list="pricing-models"
            >
            <datalist id="pricing-models">
              <option
                v-for="modelName in modelOptions"
                :key="modelName"
                :value="modelName"
              />
            </datalist>
          </article>

          <article class="rr-admin-settings__provider-field">
            <strong class="rr-admin-settings__provider-title">{{ $t('admin.pricing.fields.capability') }}</strong>
            <select v-model="form.capability">
              <option
                v-for="capability in capabilityOptions"
                :key="capability"
                :value="capability"
              >
                {{ optionLabel('admin.pricing.capabilityLabels', capability) }}
              </option>
            </select>
          </article>

          <article class="rr-admin-settings__provider-field">
            <strong class="rr-admin-settings__provider-title">{{ $t('admin.pricing.fields.billingUnit') }}</strong>
            <select v-model="form.billingUnit">
              <option
                v-for="billingUnit in billingUnitOptions"
                :key="billingUnit"
                :value="billingUnit"
              >
                {{ optionLabel('admin.pricing.billingUnitLabels', billingUnit) }}
              </option>
            </select>
          </article>

          <article class="rr-admin-settings__provider-field">
            <strong class="rr-admin-settings__provider-title">{{ $t('admin.pricing.fields.inputPrice') }}</strong>
            <input
              v-model="form.inputPrice"
              inputmode="decimal"
            >
          </article>

          <article class="rr-admin-settings__provider-field">
            <strong class="rr-admin-settings__provider-title">{{ $t('admin.pricing.fields.outputPrice') }}</strong>
            <input
              v-model="form.outputPrice"
              inputmode="decimal"
            >
          </article>

          <article class="rr-admin-settings__provider-field">
            <strong class="rr-admin-settings__provider-title">{{ $t('admin.pricing.fields.currency') }}</strong>
            <input v-model="form.currency">
          </article>

          <article class="rr-admin-settings__provider-field">
            <strong class="rr-admin-settings__provider-title">{{ $t('admin.pricing.fields.effectiveFrom') }}</strong>
            <input
              v-model="form.effectiveFrom"
              type="datetime-local"
            >
          </article>
        </div>

        <article class="rr-admin-settings__provider-field">
          <strong class="rr-admin-settings__provider-title">{{ $t('admin.pricing.fields.note') }}</strong>
          <input v-model="form.note">
          <span class="rr-admin-settings__field-hint">{{ $t('admin.pricing.noteHint') }}</span>
        </article>
      </section>

      <aside class="rr-admin-settings__side-column">
        <section class="rr-admin-settings__policy-card">
          <div class="rr-admin-settings__section-head">
            <div>
              <h4>{{ $t('admin.pricing.coverageTitle') }}</h4>
              <p>{{ $t('admin.pricing.coverageSubtitle') }}</p>
            </div>
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

          <div
            v-if="pricingWarnings.length > 0"
            class="rr-admin-settings__warning rr-admin-settings__warning--inline"
          >
            <strong>{{ $t('admin.pricing.coverageWarningsTitle') }}</strong>
            <ul class="rr-admin-settings__policy-list rr-admin-settings__policy-list--compact">
              <li
                v-for="warning in pricingWarnings"
                :key="`${warning.providerKind}:${warning.modelName}:${warning.capability}:${warning.billingUnit}`"
              >
                <strong>{{ warning.providerKind }} / {{ warning.modelName }}</strong>
                <span>
                  {{ warning.capabilityLabel }} · {{ warning.billingUnitLabel }} · {{ warning.message }}
                </span>
              </li>
            </ul>
          </div>
        </section>

        <section class="rr-admin-settings__policy-card">
          <div class="rr-admin-settings__section-head">
            <div>
              <h4>{{ $t('admin.pricing.rowsTitle') }}</h4>
              <p>{{ $t('admin.pricing.rowsSubtitle') }}</p>
            </div>
          </div>

          <div
            v-if="sortedRows.length === 0"
            class="rr-admin-settings__warning rr-admin-settings__warning--muted"
          >
            {{ $t('admin.pricing.empty') }}
          </div>

          <div
            v-else
            class="rr-admin-table"
          >
            <table>
              <thead>
                <tr>
                  <th>{{ $t('admin.pricing.headers.model') }}</th>
                  <th>{{ $t('admin.pricing.headers.scope') }}</th>
                  <th>{{ $t('admin.pricing.headers.prices') }}</th>
                  <th>{{ $t('admin.pricing.headers.window') }}</th>
                  <th>{{ $t('admin.pricing.headers.status') }}</th>
                  <th>{{ $t('admin.pricing.headers.source') }}</th>
                  <th>{{ $t('admin.pricing.headers.actions') }}</th>
                </tr>
              </thead>
              <tbody>
                <tr
                  v-for="row in sortedRows"
                  :key="row.id"
                >
                  <td>
                    <strong>{{ row.providerKind }} / {{ row.modelName }}</strong>
                    <div class="rr-admin-settings__table-meta">
                      {{ optionLabel('admin.pricing.capabilityLabels', row.capability) }}
                    </div>
                  </td>
                  <td>{{ formatScope(row) }}</td>
                  <td>
                    <div class="rr-admin-settings__table-meta">
                      {{ $t('admin.pricing.priceLine.input', { value: formatPrice(row.inputPrice, row.currency) }) }}
                    </div>
                    <div class="rr-admin-settings__table-meta">
                      {{ $t('admin.pricing.priceLine.output', { value: formatPrice(row.outputPrice, row.currency) }) }}
                    </div>
                    <div class="rr-admin-settings__table-meta">
                      {{ optionLabel('admin.pricing.billingUnitLabels', row.billingUnit) }}
                    </div>
                  </td>
                  <td>
                    <div class="rr-admin-settings__table-meta">{{ formatDateTime(row.effectiveFrom) }}</div>
                    <div class="rr-admin-settings__table-meta">{{ formatDateTime(row.effectiveTo) }}</div>
                  </td>
                  <td>{{ $t(`admin.pricing.status.${row.status}`) }}</td>
                  <td>{{ $t(`admin.pricing.source.${row.sourceKind}`) }}</td>
                  <td>
                    <div class="rr-row-actions">
                      <button
                        class="rr-button rr-button--ghost rr-button--tiny"
                        type="button"
                        :disabled="saving || row.status !== 'active'"
                        @click="startEdit(row)"
                      >
                        {{ $t('admin.pricing.edit') }}
                      </button>
                      <button
                        class="rr-button rr-button--ghost rr-button--tiny is-danger"
                        type="button"
                        :disabled="saving || row.status !== 'active'"
                        @click="emit('deactivate', row.id)"
                      >
                        {{ $t('admin.pricing.deactivate') }}
                      </button>
                    </div>
                  </td>
                </tr>
              </tbody>
            </table>
          </div>
        </section>
      </aside>
    </div>
  </section>
</template>
