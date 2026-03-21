<script setup lang="ts">
import { computed } from 'vue'
import { useDisplayFormatters } from 'src/composables/useDisplayFormatters'
import type { AdminAiConsoleState, AdminPriceCatalogEntry } from 'src/models/ui/admin'

const props = defineProps<{
  settings: AdminAiConsoleState
}>()
const { enumLabel, formatDateTime } = useDisplayFormatters()

const providerById = computed(
  () => new Map(props.settings.providers.map((provider) => [provider.id, provider])),
)

const modelById = computed(
  () => new Map(props.settings.models.map((model) => [model.id, model])),
)

const modelRows = computed(() =>
  [...props.settings.models].sort((left, right) => {
    const providerOrder =
      (providerById.value.get(left.providerCatalogId)?.displayName ?? '').localeCompare(
        providerById.value.get(right.providerCatalogId)?.displayName ?? '',
      )
    if (providerOrder !== 0) {
      return providerOrder
    }
    return left.modelName.localeCompare(right.modelName)
  }),
)

const priceRows = computed(() =>
  [...props.settings.prices].sort((left, right) =>
    right.effectiveFrom.localeCompare(left.effectiveFrom),
  ),
)

function providerLabel(modelId: string): string {
  const model = modelById.value.get(modelId)
  const provider = model ? providerById.value.get(model.providerCatalogId) : null
  return provider?.displayName ?? '—'
}

function modelLabel(price: AdminPriceCatalogEntry): string {
  const model = modelById.value.get(price.modelCatalogId)
  return model?.modelName ?? price.modelCatalogId
}

function capabilityLabel(price: AdminPriceCatalogEntry): string {
  const model = modelById.value.get(price.modelCatalogId)
  return enumLabel('admin.pricing.capabilities', model?.capabilityKind ?? null)
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
</script>

<template>
  <section class="rr-page-card rr-admin-settings">
    <header class="rr-admin-settings__header">
      <div>
        <h3>{{ $t('admin.pricing.title') }}</h3>
        <p>{{ $t('admin.pricing.subtitle') }}</p>
      </div>
      <span class="rr-status-pill is-configured">
        {{ settings.prices.length }}
      </span>
    </header>

    <div class="rr-admin-settings__layout">
      <section class="rr-admin-settings__stack-card">
        <div class="rr-admin-settings__section-head">
          <div>
            <h4>{{ $t('admin.pricing.catalogModelsTitle') }}</h4>
            <p>{{ $t('admin.pricing.catalogModelsSubtitle') }}</p>
          </div>
        </div>

        <table v-if="modelRows.length > 0">
          <thead>
            <tr>
              <th>{{ $t('admin.headers.provider') }}</th>
              <th>{{ $t('admin.headers.model') }}</th>
              <th>{{ $t('admin.headers.capability') }}</th>
              <th>{{ $t('admin.headers.modality') }}</th>
              <th>{{ $t('admin.headers.contextWindow') }}</th>
            </tr>
          </thead>
          <tbody>
            <tr
              v-for="model in modelRows"
              :key="model.id"
            >
              <td>{{ providerById.get(model.providerCatalogId)?.displayName ?? '—' }}</td>
              <td><code>{{ model.modelName }}</code></td>
              <td>{{ enumLabel('admin.pricing.capabilities', model.capabilityKind) }}</td>
              <td>{{ enumLabel('admin.pricing.modalities', model.modalityKind) }}</td>
              <td>{{ model.contextWindow ?? '—' }}</td>
            </tr>
          </tbody>
        </table>
        <p
          v-else
          class="rr-admin-table__empty"
        >
          {{ $t('admin.pricing.emptyModels') }}
        </p>
      </section>

      <section class="rr-admin-settings__stack-card">
        <div class="rr-admin-settings__section-head">
          <div>
            <h4>{{ $t('admin.pricing.catalogPricesTitle') }}</h4>
            <p>{{ $t('admin.pricing.catalogPricesSubtitle') }}</p>
          </div>
        </div>

        <table v-if="priceRows.length > 0">
          <thead>
            <tr>
              <th>{{ $t('admin.headers.provider') }}</th>
              <th>{{ $t('admin.headers.model') }}</th>
              <th>{{ $t('admin.headers.capability') }}</th>
              <th>{{ $t('admin.headers.billingUnit') }}</th>
              <th>{{ $t('admin.headers.price') }}</th>
              <th>{{ $t('admin.headers.effectiveFrom') }}</th>
            </tr>
          </thead>
          <tbody>
            <tr
              v-for="price in priceRows"
              :key="price.id"
            >
              <td>{{ providerLabel(price.modelCatalogId) }}</td>
              <td>{{ modelLabel(price) }}</td>
              <td>{{ capabilityLabel(price) }}</td>
              <td>{{ enumLabel('admin.pricing.billingUnits', price.billingUnit) }}</td>
              <td>{{ formatPrice(price) }}</td>
              <td>{{ formatDateTime(price.effectiveFrom) }}</td>
            </tr>
          </tbody>
        </table>
        <p
          v-else
          class="rr-admin-table__empty"
        >
          {{ $t('admin.pricing.empty') }}
        </p>
      </section>
    </div>
  </section>
</template>
