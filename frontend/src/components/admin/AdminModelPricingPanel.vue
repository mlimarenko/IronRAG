<script setup lang="ts">
import { computed, reactive, ref, watch } from 'vue'
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
  embedded?: boolean
}>()

const emit = defineEmits<{
  createPrice: [payload: CreateAdminPricePayload]
  updatePrice: [payload: UpdateAdminPricePayload]
}>()

const { billingUnitLabel, enumLabel, formatDateTime, priceOriginLabel } = useDisplayFormatters()

const selectedPriceKey = ref<string | null>(null)
const editorOpen = ref(false)
const pendingSubmit = ref(false)

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

const modelById = computed(
  () => new Map(props.settings.models.map((model) => [model.id, model])),
)

const priceRows = computed(() =>
  props.settings.prices
    .map((price) => {
      const model = modelById.value.get(price.modelCatalogId) ?? null
      const provider = model ? providerById.value.get(model.providerCatalogId) ?? null : null
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

const priceGroups = computed(() => {
  const groups = new Map<
    string,
    { providerName: string; rows: typeof priceRows.value }
  >()
  for (const row of priceRows.value) {
    const key = row.provider?.id ?? 'unknown'
    const current = groups.get(key)
    if (current) {
      current.rows.push(row)
      continue
    }
    groups.set(key, {
      providerName: row.provider?.displayName ?? '—',
      rows: [row],
    })
  }
  return Array.from(groups.values())
})

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

function sourceLabel(row: AdminPriceCatalogEntry): string {
  return priceOriginLabel(row.setInWorkspace)
}

function resetForm(): void {
  pendingSubmit.value = false
  editorOpen.value = true
  selectedPriceKey.value = 'price:new'
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
  resetForm()
  editorOpen.value = true
  selectedPriceKey.value = 'price:new'
}

function stagePriceForm(row: AdminPriceCatalogEntry): void {
  editorOpen.value = true
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
  <section class="rr-admin-pricing">
    <div
      class="rr-admin-pricing__layout"
      :class="{ 'rr-admin-pricing__layout--editing': editorOpen }"
    >
      <aside class="rr-admin-pricing__rail">
        <header class="rr-admin-pricing__rail-head">
          <div>
            <h3>{{ $t('admin.pricing.catalogPricesTitle') }}</h3>
            <p>{{ $t('admin.pricing.catalogPricesSubtitle') }}</p>
          </div>
          <button
            class="rr-button"
            type="button"
            @click="openCreateForm"
          >
            {{ $t('admin.pricing.setPrice') }}
          </button>
        </header>

        <p
          v-if="!editorOpen"
          class="rr-admin-pricing__intro"
        >
          {{ $t('admin.pricing.editorSubtitle') }}
        </p>

        <div
          v-if="priceGroups.length"
          class="rr-admin-pricing__group-stack"
        >
          <section
            v-for="group in priceGroups"
            :key="group.providerName"
            class="rr-admin-pricing__group"
          >
            <header class="rr-admin-pricing__group-head">
              <strong>{{ group.providerName }}</strong>
            </header>

            <div class="rr-admin-pricing__group-list">
                <button
                v-for="row in group.rows"
                :key="row.id"
                class="rr-admin-pricing__row"
                :class="{
                  'rr-admin-pricing__row--active': selectedPriceKey === `price:${row.id}`,
                  'rr-admin-pricing__row--current': row.setInWorkspace,
                }"
                type="button"
                @click="stagePriceForm(row)"
              >
                <span class="rr-admin-pricing__row-title">
                  {{ row.model?.modelName ?? '—' }}
                </span>
                <span class="rr-admin-pricing__row-meta">
                  {{ billingUnitLabel(row.billingUnit) }}
                </span>
                <div class="rr-admin-pricing__row-trailing">
                  <strong>{{ formatPrice(row) }}</strong>
                  <span>{{ sourceLabel(row) }}</span>
                </div>
              </button>
            </div>
          </section>
        </div>

        <p
          v-else
          class="rr-admin-pricing__empty-copy"
        >
          {{ $t('admin.pricing.empty') }}
        </p>
      </aside>

      <section
        v-if="editorOpen"
        class="rr-admin-pricing__detail"
      >
        <div class="rr-admin-pricing__detail-card">
          <header class="rr-admin-pricing__detail-head">
            <div>
              <h3>{{ form.priceId ? $t('admin.pricing.editPrice') : $t('admin.pricing.setPrice') }}</h3>
              <p>{{ $t('admin.pricing.subtitle') }}</p>
            </div>
          </header>

          <p
            v-if="errorMessage"
            class="rr-admin-pricing__detail-error"
          >
            {{ errorMessage }}
          </p>

          <div class="rr-admin-pricing__form-grid">
            <label class="rr-admin-pricing__field rr-admin-pricing__field--wide">
              <span>{{ $t('admin.headers.model') }}</span>
              <select v-model="form.modelCatalogId">
                <option
                  v-for="model in settings.models"
                  :key="model.id"
                  :value="model.id"
                >
                  {{ modelDescriptor(model.id) }}
                </option>
              </select>
            </label>

            <label class="rr-admin-pricing__field">
              <span>{{ $t('admin.headers.billingUnit') }}</span>
              <select v-model="form.billingUnit">
                <option value="per_1m_input_tokens">
                  {{ enumLabel('admin.pricing.billingUnits', 'per_1m_input_tokens') }}
                </option>
                <option value="per_1m_output_tokens">
                  {{ enumLabel('admin.pricing.billingUnits', 'per_1m_output_tokens') }}
                </option>
              </select>
            </label>

            <label class="rr-admin-pricing__field">
              <span>{{ $t('admin.headers.price') }}</span>
              <input
                v-model="form.unitPrice"
                type="number"
                step="0.000001"
                min="0"
              >
            </label>

            <label class="rr-admin-pricing__field">
              <span>{{ $t('admin.pricing.currencyCode') }}</span>
              <input
                v-model="form.currencyCode"
                type="text"
                maxlength="8"
              >
            </label>

            <label class="rr-admin-pricing__field">
              <span>{{ $t('admin.headers.effectiveFrom') }}</span>
              <input
                v-model="form.effectiveFrom"
                type="datetime-local"
              >
            </label>

            <label class="rr-admin-pricing__field">
              <span>{{ $t('admin.pricing.effectiveTo') }}</span>
              <input
                v-model="form.effectiveTo"
                type="datetime-local"
              >
            </label>
          </div>

          <div
            v-if="selectedPriceKey && selectedPriceKey !== 'price:new'"
            class="rr-admin-pricing__detail-meta"
          >
            <span>{{ effectivePeriodLabel(priceRows.find((row) => `price:${row.id}` === selectedPriceKey)!) }}</span>
          </div>

          <div class="rr-admin-pricing__actions">
            <button
              class="rr-button rr-button--ghost"
              type="button"
              @click="resetForm"
            >
              {{ $t('dialogs.close') }}
            </button>
            <button
              class="rr-button"
              type="button"
              :disabled="!canSave || saving"
              @click="submit"
            >
              {{
                form.priceId
                  ? $t('admin.pricing.updatePrice')
                  : $t('admin.pricing.setPrice')
              }}
            </button>
          </div>
        </div>
      </section>
    </div>
  </section>
</template>

<style scoped>
.rr-admin-pricing {
  display: grid;
}

.rr-admin-pricing__layout {
  display: grid;
  gap: 1rem;
  grid-template-columns: 1fr;
  min-height: 0;
}

.rr-admin-pricing__layout--editing {
  grid-template-columns: minmax(360px, 0.95fr) minmax(0, 1.2fr);
  min-height: 34rem;
}

.rr-admin-pricing__rail,
.rr-admin-pricing__detail {
  border: 1px solid var(--rr-border-soft);
  border-radius: 22px;
  background: rgba(255, 255, 255, 0.72);
}

.rr-admin-pricing__rail {
  display: grid;
  gap: 0.9rem;
  align-content: start;
  padding: 1.05rem;
}

.rr-admin-pricing__rail-head,
.rr-admin-pricing__detail-head {
  display: flex;
  justify-content: space-between;
  gap: 1rem;
  align-items: flex-start;
}

.rr-admin-pricing__rail-head h3,
.rr-admin-pricing__detail-head h3 {
  margin: 0;
  font-size: 1.14rem;
  color: var(--rr-text-primary);
}

.rr-admin-pricing__rail-head p,
.rr-admin-pricing__detail-head p {
  margin: 0.2rem 0 0;
  font-size: 0.95rem;
  line-height: 1.5;
  color: var(--rr-text-secondary);
}

.rr-admin-pricing__group-stack {
  display: grid;
  gap: 0.85rem;
}

.rr-admin-pricing__intro {
  margin: 0;
  padding: 0.95rem 1rem;
  border: 1px solid var(--rr-border-muted);
  border-radius: 16px;
  background: rgba(248, 250, 252, 0.78);
  color: var(--rr-text-secondary);
  font-size: 0.9rem;
  line-height: 1.5;
}

.rr-admin-pricing__group {
  display: grid;
  gap: 0.45rem;
}

.rr-admin-pricing__group-head {
  color: var(--rr-text-secondary);
  font-size: 0.92rem;
}

.rr-admin-pricing__group-list {
  display: grid;
  gap: 0.45rem;
}

.rr-admin-pricing__row {
  width: 100%;
  border: 1px solid var(--rr-border-muted);
  border-radius: 16px;
  background: rgba(255, 255, 255, 0.78);
  padding: 0.9rem 1rem;
  text-align: left;
  display: grid;
  gap: 0.28rem;
  transition:
    border-color 120ms ease,
    background-color 120ms ease;
}

.rr-admin-pricing__row:hover,
.rr-admin-pricing__row--active {
  border-color: rgba(56, 87, 255, 0.18);
  background: rgba(244, 247, 255, 0.96);
}

.rr-admin-pricing__row--current {
  border-left: 3px solid rgba(56, 87, 255, 0.35);
}

.rr-admin-pricing__row-title,
.rr-admin-pricing__row-trailing strong {
  color: var(--rr-text-primary);
  font-weight: 600;
  font-size: 0.98rem;
}

.rr-admin-pricing__row-meta,
.rr-admin-pricing__row-trailing span,
.rr-admin-pricing__empty-copy,
.rr-admin-pricing__detail-meta {
  font-size: 0.92rem;
  line-height: 1.5;
  color: var(--rr-text-secondary);
}

.rr-admin-pricing__row-trailing {
  display: flex;
  flex-wrap: wrap;
  gap: 0.45rem 0.75rem;
  align-items: center;
}

.rr-admin-pricing__detail {
  padding: 1.05rem;
}

.rr-admin-pricing__detail-empty,
.rr-admin-pricing__detail-card {
  height: 100%;
  border: 1px solid var(--rr-border-muted);
  border-radius: 18px;
  background: rgba(248, 250, 252, 0.72);
  padding: 1.1rem;
}

.rr-admin-pricing__detail-empty {
  display: grid;
  align-content: center;
  gap: 0.45rem;
  text-align: center;
}

.rr-admin-pricing__detail-empty strong {
  color: var(--rr-text-primary);
  font-size: 1rem;
}

.rr-admin-pricing__detail-empty p {
  margin: 0;
  color: var(--rr-text-secondary);
}

.rr-admin-pricing__detail-card {
  display: grid;
  gap: 1.1rem;
  align-content: start;
}

.rr-admin-pricing__form-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 0.9rem;
}

.rr-admin-pricing__field {
  display: grid;
  gap: 0.35rem;
}

.rr-admin-pricing__field--wide {
  grid-column: 1 / -1;
}

.rr-admin-pricing__field span {
  font-size: 0.86rem;
  font-weight: 600;
  color: var(--rr-text-secondary);
}

.rr-admin-pricing__field input,
.rr-admin-pricing__field select {
  width: 100%;
  border: 1px solid var(--rr-border-soft);
  border-radius: 14px;
  background: #fff;
  min-height: 2.65rem;
  padding: 0.8rem 0.95rem;
  font-size: 0.95rem;
  color: var(--rr-text-primary);
}

.rr-admin-pricing__actions {
  display: flex;
  flex-wrap: wrap;
  justify-content: flex-end;
  gap: 0.75rem;
}

.rr-admin-pricing__detail-error {
  margin: 0;
  padding: 0.75rem 0.85rem;
  border-radius: 14px;
  background: rgba(254, 242, 242, 0.92);
  border: 1px solid rgba(248, 113, 113, 0.22);
  color: #b91c1c;
  font-size: 0.9rem;
  line-height: 1.45;
}

@media (max-width: 1024px) {
  .rr-admin-pricing__layout {
    grid-template-columns: 1fr;
    min-height: 0;
  }

  .rr-admin-pricing__form-grid {
    grid-template-columns: 1fr;
  }
}
</style>
