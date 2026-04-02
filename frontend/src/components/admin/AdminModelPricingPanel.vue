<script setup lang="ts">
import { computed, reactive, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import SearchField from 'src/components/design-system/SearchField.vue'
import { useDisplayFormatters } from 'src/composables/useDisplayFormatters'
import type {
  AdminAiConsoleState,
  AdminPriceCatalogEntry,
  CreateAdminPricePayload,
  UpdateAdminPricePayload,
} from 'src/models/ui/admin'

type EditablePriceForm = {
  priceId: string | null
  workspaceId: string
  modelCatalogId: string
  billingUnit: string
  unitPrice: string
  currencyCode: string
  effectiveFrom: string
  effectiveTo: string
}

const props = defineProps<{
  settings: AdminAiConsoleState
  saving: boolean
  commitVersion: number
  errorMessage?: string | null
}>()

const emit = defineEmits<{
  createPrice: [payload: CreateAdminPricePayload]
  updatePrice: [payload: UpdateAdminPricePayload]
}>()

const { t } = useI18n()
const { billingUnitLabel, enumLabel, formatDateTime, priceOriginLabel } = useDisplayFormatters()

const selectedPriceKey = ref<string | null>(null)
const pendingSubmit = ref(false)
const searchQuery = ref('')
const selectedProviderId = ref<string>('all')

const form = reactive<EditablePriceForm>({
  priceId: null,
  workspaceId: '',
  modelCatalogId: '',
  billingUnit: 'per_1m_input_tokens',
  unitPrice: '',
  currencyCode: 'USD',
  effectiveFrom: '',
  effectiveTo: '',
})

watch(
  () => props.settings,
  (settings) => {
    form.workspaceId = settings.workspaceId
    if (!settings.models.some((model) => model.id === form.modelCatalogId)) {
      form.modelCatalogId = settings.models[0]?.id ?? ''
    }
  },
  { immediate: true },
)

watch(
  () => `${props.settings.workspaceId}:${props.settings.libraryId}`,
  (nextKey, previousKey) => {
    if (!previousKey || nextKey === previousKey) {
      return
    }
    resetForm()
  },
)

const providerById = computed(
  () => new Map(props.settings.providers.map((provider) => [provider.id, provider])),
)

const modelById = computed(() => new Map(props.settings.models.map((model) => [model.id, model])))

const priceRows = computed(() =>
  props.settings.prices
    .map((price) => {
      const model = modelById.value.get(price.modelCatalogId) ?? null
      const provider = model ? (providerById.value.get(model.providerCatalogId) ?? null) : null
      return { ...price, model, provider }
    })
    .sort((left, right) => {
      const providerOrder = (left.provider?.displayName ?? '').localeCompare(
        right.provider?.displayName ?? '',
      )
      if (providerOrder !== 0) {
        return providerOrder
      }
      const modelOrder = (left.model?.modelName ?? '').localeCompare(right.model?.modelName ?? '')
      if (modelOrder !== 0) {
        return modelOrder
      }
      return right.effectiveFrom.localeCompare(left.effectiveFrom)
    }),
)

const filteredPriceRows = computed(() => {
  const query = searchQuery.value.trim().toLowerCase()
  return priceRows.value.filter((row) => {
    if (selectedProviderId.value !== 'all' && row.provider?.id !== selectedProviderId.value) {
      return false
    }

    if (!query) {
      return true
    }

    const haystack = [
      row.provider?.displayName ?? '',
      row.model?.modelName ?? '',
      row.currencyCode,
      row.billingUnit,
      row.priceVariantKey,
      tierLabel(row),
      billingUnitLabel(row.billingUnit),
      sourceLabel(row),
      formatPrice(row),
    ]
      .join(' ')
      .toLowerCase()
    return haystack.includes(query)
  })
})

const providerFilters = computed(() => [
  {
    id: 'all',
    label: t('admin.pricing.providers.all'),
    count: priceRows.value.length,
  },
  ...props.settings.providers.map((provider) => ({
    id: provider.id,
    label: provider.displayName,
    count: priceRows.value.filter((row) => row.provider?.id === provider.id).length,
  })),
])

const showProviderInRows = computed(
  () =>
    selectedProviderId.value === 'all' &&
    props.settings.providers.filter((provider) =>
      priceRows.value.some((row) => row.provider?.id === provider.id),
    ).length > 1,
)

const selectedPrice = computed(
  () => priceRows.value.find((row) => `price:${row.id}` === selectedPriceKey.value) ?? null,
)

const billingUnitOptions = computed(() => [
  'per_1m_input_tokens',
  'per_1m_cached_input_tokens',
  'per_1m_output_tokens',
])

const canSave = computed(
  () =>
    form.workspaceId.trim().length > 0 &&
    form.modelCatalogId.trim().length > 0 &&
    props.settings.models.some((model) => model.id === form.modelCatalogId) &&
    form.billingUnit.trim().length > 0 &&
    form.unitPrice.trim().length > 0 &&
    form.currencyCode.trim().length > 0 &&
    form.effectiveFrom.trim().length > 0,
)

function toLocalInput(value: string | null): string {
  if (!value) {
    return ''
  }
  const date = new Date(value)
  if (Number.isNaN(date.getTime())) {
    return ''
  }
  const offset = date.getTimezoneOffset()
  const local = new Date(date.getTime() - offset * 60_000)
  return local.toISOString().slice(0, 16)
}

function fromLocalInput(value: string): string | null {
  const normalized = value.trim()
  if (!normalized) {
    return null
  }
  const date = new Date(normalized)
  return Number.isNaN(date.getTime()) ? null : date.toISOString()
}

function formatPrice(row: AdminPriceCatalogEntry): string {
  const parsed = Number(row.unitPrice)
  if (!Number.isFinite(parsed)) {
    return `${row.unitPrice} ${row.currencyCode}`
  }
  try {
    return new Intl.NumberFormat(undefined, {
      style: 'currency',
      currency: row.currencyCode,
      maximumFractionDigits: 6,
    }).format(parsed)
  } catch {
    return `${row.unitPrice} ${row.currencyCode}`
  }
}

function modelDescriptor(modelCatalogId: string): string {
  const model = modelById.value.get(modelCatalogId)
  if (!model) {
    return modelCatalogId
  }
  const provider = providerById.value.get(model.providerCatalogId)
  return provider ? `${provider.displayName} · ${model.modelName}` : model.modelName
}

function effectivePeriodLabel(row: AdminPriceCatalogEntry): string {
  if (!row.effectiveTo) {
    return formatDateTime(row.effectiveFrom)
  }
  return `${formatDateTime(row.effectiveFrom)} → ${formatDateTime(row.effectiveTo)}`
}

function tierLabel(row: AdminPriceCatalogEntry): string {
  if (row.requestInputTokensMin == null && row.requestInputTokensMax == null) {
    return ''
  }
  const lower =
    row.requestInputTokensMin == null || row.requestInputTokensMin <= 0
      ? '0'
      : new Intl.NumberFormat().format(row.requestInputTokensMin)
  const upper =
    row.requestInputTokensMax == null
      ? '∞'
      : new Intl.NumberFormat().format(row.requestInputTokensMax)
  return `${lower}–${upper} input`
}

function variantLabel(row: AdminPriceCatalogEntry): string {
  if (!row.priceVariantKey || row.priceVariantKey === 'default') {
    return ''
  }
  return row.priceVariantKey.replace(/_/g, ' ')
}

function sourceLabel(row: AdminPriceCatalogEntry): string {
  return priceOriginLabel(row.setInWorkspace)
}

function resetForm(): void {
  pendingSubmit.value = false
  form.priceId = null
  form.workspaceId = props.settings.workspaceId
  form.modelCatalogId = props.settings.models[0]?.id ?? ''
  form.billingUnit = 'per_1m_input_tokens'
  form.unitPrice = ''
  form.currencyCode = 'USD'
  form.effectiveFrom = toLocalInput(new Date().toISOString())
  form.effectiveTo = ''
}

function openCreateForm(): void {
  selectedPriceKey.value = 'price:new'
  resetForm()
}

function stagePriceForm(row: AdminPriceCatalogEntry): void {
  selectedPriceKey.value = `price:${row.id}`
  form.priceId = row.setInWorkspace ? row.id : null
  form.workspaceId = props.settings.workspaceId
  form.modelCatalogId = row.modelCatalogId
  form.billingUnit = row.billingUnit
  form.unitPrice = row.unitPrice
  form.currencyCode = row.currencyCode
  form.effectiveFrom = toLocalInput(row.effectiveFrom)
  form.effectiveTo = toLocalInput(row.effectiveTo)
}

function submit(): void {
  if (!canSave.value) {
    return
  }
  const effectiveFrom = fromLocalInput(form.effectiveFrom)
  if (!effectiveFrom) {
    return
  }
  const payload = {
    modelCatalogId: form.modelCatalogId,
    billingUnit: form.billingUnit,
    unitPrice: form.unitPrice.trim(),
    currencyCode: form.currencyCode.trim().toUpperCase(),
    effectiveFrom,
    effectiveTo: fromLocalInput(form.effectiveTo),
  }

  if (form.priceId) {
    emit('updatePrice', {
      priceId: form.priceId,
      ...payload,
    })
  } else {
    emit('createPrice', {
      workspaceId: form.workspaceId,
      ...payload,
    })
  }
  pendingSubmit.value = true
}

resetForm()

watch(
  filteredPriceRows,
  (rows) => {
    if (rows.length === 0) {
      if (searchQuery.value.trim().length > 0) {
        selectedPriceKey.value = null
        return
      }
      openCreateForm()
      return
    }
    if (selectedPriceKey.value === 'price:new') {
      return
    }
    if (!rows.some((row) => `price:${row.id}` === selectedPriceKey.value)) {
      stagePriceForm(rows[0])
    }
  },
  { immediate: true },
)

watch(
  () => props.commitVersion,
  (next, previous) => {
    if (next <= previous || !pendingSubmit.value) {
      return
    }
    pendingSubmit.value = false
    openCreateForm()
  },
)
</script>

<template>
  <section class="rr-admin-workbench rr-admin-workbench--pricing">
    <div class="rr-admin-workbench__layout">
      <aside class="rr-admin-workbench__rail">
        <header class="rr-admin-workbench__pane-head">
          <div class="rr-admin-workbench__pane-copy">
            <h3>{{ $t('admin.pricing.catalogPricesTitle') }}</h3>
            <p>{{ $t('admin.pricing.catalogPricesSubtitle') }}</p>
          </div>
          <button class="rr-button" type="button" @click="openCreateForm">
            {{ $t('admin.pricing.newDraft') }}
          </button>
        </header>

        <SearchField
          v-model="searchQuery"
          :placeholder="$t('admin.pricing.searchPlaceholder')"
          @clear="searchQuery = ''"
        />

        <div v-if="providerFilters.length > 1" class="rr-admin-pricing__provider-filters">
          <button
            v-for="provider in providerFilters"
            :key="provider.id"
            type="button"
            class="rr-admin-pricing__provider-filter"
            :class="{ 'is-active': selectedProviderId === provider.id }"
            @click="selectedProviderId = provider.id"
          >
            <span>{{ provider.label }}</span>
            <strong>{{ provider.count }}</strong>
          </button>
        </div>

        <p
          v-if="errorMessage"
          class="rr-admin-workbench__feedback rr-admin-workbench__feedback--error"
        >
          {{ errorMessage }}
        </p>

        <div v-if="filteredPriceRows.length" class="rr-admin-pricing__catalog-list">
          <button
            v-for="row in filteredPriceRows"
            :key="row.id"
            class="rr-admin-workbench__row"
            :class="{
              'rr-admin-workbench__row--active': selectedPriceKey === `price:${row.id}`,
              'rr-admin-workbench__row--accent': row.setInWorkspace,
            }"
            type="button"
            @click="stagePriceForm(row)"
          >
            <div class="rr-admin-workbench__row-head">
              <strong>{{ row.model?.modelName ?? '—' }}</strong>
              <span class="rr-status-pill" :class="row.setInWorkspace ? 'is-success' : 'is-muted'">
                {{ sourceLabel(row) }}
              </span>
            </div>
            <span class="rr-admin-workbench__row-subtitle">
              <template v-if="showProviderInRows">
                {{ row.provider?.displayName ?? '—' }} ·
              </template>
              {{ billingUnitLabel(row.billingUnit) }}
              <template v-if="variantLabel(row) || tierLabel(row)">
                ·
                {{
                  [variantLabel(row), tierLabel(row)].filter((part) => part.length > 0).join(' · ')
                }}
              </template>
            </span>
            <div class="rr-admin-workbench__row-trailing">
              <strong>{{ formatPrice(row) }}</strong>
              <span>{{ effectivePeriodLabel(row) }}</span>
            </div>
          </button>
        </div>

        <p v-else-if="priceRows.length === 0" class="rr-admin-workbench__state">
          {{ $t('admin.pricing.empty') }}
        </p>
        <p v-else class="rr-admin-workbench__state">
          {{ $t('shared.feedbackState.noResults') }}
        </p>
      </aside>

      <section class="rr-admin-workbench__detail">
        <div class="rr-admin-workbench__detail-card">
          <header class="rr-admin-workbench__detail-head">
            <div class="rr-admin-workbench__pane-copy">
              <h3>
                {{ form.priceId ? $t('admin.pricing.editPrice') : $t('admin.pricing.setPrice') }}
              </h3>
              <p>
                {{
                  selectedPrice
                    ? modelDescriptor(selectedPrice.modelCatalogId)
                    : $t('admin.pricing.editorSubtitle')
                }}
              </p>
            </div>
          </header>

          <div v-if="selectedPrice" class="rr-admin-pricing__selection-chips">
            <span class="rr-admin-pricing__selection-chip">
              {{ selectedPrice.provider?.displayName ?? '—' }}
            </span>
            <span class="rr-admin-pricing__selection-chip">
              {{ billingUnitLabel(selectedPrice.billingUnit) }}
            </span>
            <span
              v-if="variantLabel(selectedPrice) || tierLabel(selectedPrice)"
              class="rr-admin-pricing__selection-chip"
            >
              {{
                [variantLabel(selectedPrice), tierLabel(selectedPrice)]
                  .filter((part) => part.length > 0)
                  .join(' · ')
              }}
            </span>
            <span class="rr-admin-pricing__selection-chip">
              {{ sourceLabel(selectedPrice) }}
            </span>
          </div>
          <p v-else class="rr-admin-workbench__feedback rr-admin-workbench__feedback--info">
            {{ $t('admin.pricing.newDraftHint') }}
          </p>

          <div class="rr-admin-pricing__form-grid">
            <label class="rr-admin-pricing__field rr-admin-pricing__field--wide">
              <span>{{ $t('admin.headers.model') }}</span>
              <select v-model="form.modelCatalogId">
                <option v-for="model in settings.models" :key="model.id" :value="model.id">
                  {{ modelDescriptor(model.id) }}
                </option>
              </select>
            </label>

            <label class="rr-admin-pricing__field">
              <span>{{ $t('admin.headers.billingUnit') }}</span>
              <select v-model="form.billingUnit">
                <option v-for="unit in billingUnitOptions" :key="unit" :value="unit">
                  {{ enumLabel('admin.pricing.billingUnits', unit) }}
                </option>
              </select>
            </label>

            <label class="rr-admin-pricing__field">
              <span>{{ $t('admin.headers.price') }}</span>
              <input v-model="form.unitPrice" type="number" step="0.000001" min="0" />
            </label>

            <label class="rr-admin-pricing__field">
              <span>{{ $t('admin.pricing.currencyCode') }}</span>
              <input v-model="form.currencyCode" type="text" maxlength="8" />
            </label>

            <label class="rr-admin-pricing__field">
              <span>{{ $t('admin.headers.effectiveFrom') }}</span>
              <input v-model="form.effectiveFrom" type="datetime-local" />
            </label>

            <label class="rr-admin-pricing__field">
              <span>{{ $t('admin.pricing.effectiveTo') }}</span>
              <input v-model="form.effectiveTo" type="datetime-local" />
            </label>
          </div>

          <div class="rr-admin-workbench__detail-actions">
            <button class="rr-button rr-button--ghost" type="button" @click="resetForm">
              {{ $t('admin.pricing.newDraft') }}
            </button>
            <button class="rr-button" type="button" :disabled="!canSave || saving" @click="submit">
              {{ form.priceId ? $t('admin.pricing.updatePrice') : $t('admin.pricing.setPrice') }}
            </button>
          </div>
        </div>
      </section>
    </div>
  </section>
</template>

<style scoped>
.rr-admin-pricing__catalog-meta,
.rr-admin-pricing__selection-meta {
  margin: 0;
  color: var(--rr-text-secondary);
  font-size: 0.78rem;
  line-height: 1.5;
}

.rr-admin-pricing__selection-chips {
  display: flex;
  flex-wrap: wrap;
  gap: 0.42rem;
}

.rr-admin-pricing__selection-chip {
  display: inline-flex;
  align-items: center;
  min-height: 1.85rem;
  padding: 0.22rem 0.56rem;
  border-radius: 999px;
  border: 1px solid rgba(226, 232, 240, 0.86);
  background: rgba(248, 250, 252, 0.82);
  color: var(--rr-text-secondary);
  font-size: 0.74rem;
  line-height: 1.2;
}

.rr-admin-pricing__provider-filters {
  display: flex;
  flex-wrap: wrap;
  gap: 0.45rem;
}

.rr-admin-pricing__provider-filter {
  display: inline-flex;
  align-items: center;
  gap: 0.45rem;
  min-height: 1.9rem;
  padding: 0.3rem 0.62rem;
  border: 1px solid rgba(203, 213, 225, 0.86);
  border-radius: 999px;
  background: rgba(255, 255, 255, 0.82);
  color: var(--rr-text-secondary);
  font-size: 0.76rem;
  font-weight: 600;
  cursor: pointer;
}

.rr-admin-pricing__provider-filter strong {
  color: var(--rr-text-primary);
  font-size: 0.7rem;
}

.rr-admin-pricing__provider-filter.is-active {
  color: #334155;
  border-color: rgba(99, 102, 241, 0.24);
  background: rgba(244, 247, 255, 0.98);
}

.rr-admin-pricing__catalog-list {
  max-height: min(62vh, 46rem);
  overflow: auto;
  display: grid;
  gap: 0.5rem;
  padding-right: 2px;
}

.rr-admin-workbench--pricing .rr-admin-workbench__layout {
  grid-template-columns: minmax(310px, 360px) minmax(0, 1fr);
}

.rr-admin-pricing__catalog-list :deep(.rr-admin-workbench__row) {
  gap: 0.36rem;
  padding: 0.76rem 0.8rem;
}

.rr-admin-pricing__catalog-list :deep(.rr-admin-workbench__row-head) {
  align-items: center;
}

.rr-admin-pricing__catalog-list :deep(.rr-admin-workbench__row-subtitle) {
  line-height: 1.35;
}

.rr-admin-pricing__catalog-list :deep(.rr-admin-workbench__row-trailing) {
  align-items: baseline;
}

.rr-admin-workbench--pricing .rr-admin-workbench__detail-card {
  position: sticky;
  top: 0;
}

.rr-admin-pricing__form-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 12px;
}

.rr-admin-pricing__field {
  display: grid;
  gap: 6px;
}

.rr-admin-pricing__field--wide {
  grid-column: 1 / -1;
}

.rr-admin-pricing__field span {
  color: var(--rr-text-muted);
  font-size: 0.74rem;
  font-weight: 700;
  letter-spacing: 0.06em;
  text-transform: uppercase;
}

@media (max-width: 820px) {
  .rr-admin-pricing__form-grid {
    grid-template-columns: minmax(0, 1fr);
  }
}
</style>
