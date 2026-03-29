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
  workspaceName: string
  libraryName: string
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

const filteredPriceRows = computed(() => {
  const query = searchQuery.value.trim().toLowerCase()
  if (!query) {
    return priceRows.value
  }
  return priceRows.value.filter((row) => {
    const haystack = [
      row.provider?.displayName ?? '',
      row.model?.modelName ?? '',
      row.currencyCode,
      row.billingUnit,
      billingUnitLabel(row.billingUnit),
      sourceLabel(row),
      formatPrice(row),
    ]
      .join(' ')
      .toLowerCase()
    return haystack.includes(query)
  })
})

const priceGroups = computed(() => {
  const groups = new Map<
    string,
    { providerName: string; rows: typeof filteredPriceRows.value }
  >()
  for (const row of filteredPriceRows.value) {
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

const selectedPrice = computed(
  () => priceRows.value.find((row) => `price:${row.id}` === selectedPriceKey.value) ?? null,
)

const summary = computed(() => ({
  total: priceRows.value.length,
  current: priceRows.value.filter((row) => row.setInWorkspace).length,
  providers: new Set(priceRows.value.map((row) => row.provider?.id ?? 'unknown')).size,
}))

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
          <button
            class="rr-button"
            type="button"
            @click="openCreateForm"
          >
            {{ $t('admin.pricing.setPrice') }}
          </button>
        </header>

        <div class="rr-admin-workbench__context">
          <div class="rr-admin-workbench__context-chip">
            <span>{{ $t('shell.workspace') }}</span>
            <strong>{{ workspaceName }}</strong>
          </div>
          <div class="rr-admin-workbench__context-chip">
            <span>{{ $t('shell.library') }}</span>
            <strong>{{ libraryName }}</strong>
          </div>
        </div>

        <SearchField
          v-model="searchQuery"
          :placeholder="$t('admin.pricing.searchPlaceholder')"
          @clear="searchQuery = ''"
        />

        <div class="rr-admin-workbench__summary">
          <article class="rr-admin-workbench__metric">
            <strong>{{ summary.total }}</strong>
            <span>{{ $t('admin.pricing.summary.total') }}</span>
          </article>
          <article class="rr-admin-workbench__metric">
            <strong>{{ summary.current }}</strong>
            <span>{{ $t('admin.pricing.summary.current') }}</span>
          </article>
          <article class="rr-admin-workbench__metric">
            <strong>{{ summary.providers }}</strong>
            <span>{{ $t('admin.pricing.summary.providers') }}</span>
          </article>
        </div>

        <p
          v-if="errorMessage"
          class="rr-admin-workbench__feedback rr-admin-workbench__feedback--error"
        >
          {{ errorMessage }}
        </p>

        <div
          v-if="priceGroups.length"
          class="rr-admin-workbench__group-stack"
        >
          <section
            v-for="group in priceGroups"
            :key="group.providerName"
            class="rr-admin-workbench__group"
          >
            <header class="rr-admin-workbench__group-head">
              <strong>{{ group.providerName }}</strong>
              <span>{{ group.rows.length }}</span>
            </header>

            <div class="rr-admin-workbench__group-list">
              <button
                v-for="row in group.rows"
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
                  <span
                    class="rr-status-pill"
                    :class="row.setInWorkspace ? 'is-success' : 'is-muted'"
                  >
                    {{ sourceLabel(row) }}
                  </span>
                </div>
                <span class="rr-admin-workbench__row-subtitle">
                  {{ billingUnitLabel(row.billingUnit) }}
                </span>
                <div class="rr-admin-workbench__row-meta">
                  <span>{{ effectivePeriodLabel(row) }}</span>
                </div>
                <div class="rr-admin-workbench__row-trailing">
                  <strong>{{ formatPrice(row) }}</strong>
                </div>
              </button>
            </div>
          </section>
        </div>

        <p
          v-else-if="priceRows.length === 0"
          class="rr-admin-workbench__state"
        >
          {{ $t('admin.pricing.empty') }}
        </p>
        <p
          v-else
          class="rr-admin-workbench__state"
        >
          {{ $t('shared.feedbackState.noResults') }}
        </p>
      </aside>

      <section class="rr-admin-workbench__detail">
        <div class="rr-admin-workbench__detail-card">
          <header class="rr-admin-workbench__detail-head">
            <div class="rr-admin-workbench__pane-copy">
              <h3>{{ form.priceId ? $t('admin.pricing.editPrice') : $t('admin.pricing.setPrice') }}</h3>
              <p>
                {{
                  selectedPrice
                    ? modelDescriptor(selectedPrice.modelCatalogId)
                    : $t('admin.pricing.editorSubtitle')
                }}
              </p>
            </div>
          </header>

          <dl
            v-if="selectedPrice"
            class="rr-admin-workbench__detail-grid"
          >
            <div>
              <dt>{{ $t('admin.headers.provider') }}</dt>
              <dd>{{ selectedPrice.provider?.displayName ?? '—' }}</dd>
            </div>
            <div>
              <dt>{{ $t('admin.headers.billingUnit') }}</dt>
              <dd>{{ billingUnitLabel(selectedPrice.billingUnit) }}</dd>
            </div>
            <div>
              <dt>{{ $t('admin.headers.price') }}</dt>
              <dd>{{ formatPrice(selectedPrice) }}</dd>
            </div>
            <div>
              <dt>{{ $t('admin.pricing.scheduleTitle') }}</dt>
              <dd>{{ effectivePeriodLabel(selectedPrice) }}</dd>
            </div>
          </dl>
          <p
            v-else
            class="rr-admin-workbench__feedback rr-admin-workbench__feedback--info"
          >
            {{ $t('admin.pricing.newDraftHint') }}
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

          <div class="rr-admin-workbench__detail-actions">
            <button
              class="rr-button rr-button--ghost"
              type="button"
              @click="resetForm"
            >
              {{ $t('admin.pricing.newDraft') }}
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
  min-height: 2.625rem;
  padding: 0.65rem 0.95rem;
  font-size: 0.92rem;
  color: var(--rr-text-primary);
  transition: border-color 150ms ease, box-shadow 150ms ease;
}

.rr-admin-pricing__field input:focus,
.rr-admin-pricing__field select:focus {
  border-color: var(--rr-accent);
  box-shadow: 0 0 0 3px var(--rr-accent-muted);
  outline: none;
}

@media (max-width: 1024px) {
  .rr-admin-pricing__form-grid {
    grid-template-columns: 1fr;
  }
}
</style>
