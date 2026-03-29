<script setup lang="ts">
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import type { DocumentUploadFailure, LibraryCostSummary } from 'src/models/ui/documents'
import UploadDropzone from './UploadDropzone.vue'

const props = defineProps<{
  acceptedFormats: string[]
  maxSizeMb: number
  loading: boolean
  totalCount?: number
  activeCount?: number
  failedCount?: number
  readyCount?: number
  costSummary?: LibraryCostSummary | null
  uploadFailures: DocumentUploadFailure[]
  hasDocuments?: boolean
}>()

const emit = defineEmits<{
  select: [files: File[]]
  clearFailures: []
}>()

const { t } = useI18n()
const uploadRef = ref<InstanceType<typeof UploadDropzone> | null>(null)

const uploadFailureSummary = computed(() => {
  const count = props.uploadFailures.length
  if (count === 0) return null
  return t('documents.uploadReport.summary', { count })
})

const hasActiveDocuments = computed(() => (props.activeCount ?? 0) > 0)
const hasFailedDocuments = computed(() => (props.failedCount ?? 0) > 0)
const hasCostSummary = computed(() => Boolean(props.costSummary && props.costSummary.totalCost > 0))
const showTotalStat = computed(() => (props.totalCount ?? 0) > 0 || Boolean(props.hasDocuments))
const showAvgCostStat = computed(
  () => hasCostSummary.value && (hasActiveDocuments.value || hasFailedDocuments.value),
)
const showReadyStat = computed(() => {
  const readyCount = props.readyCount ?? 0
  const totalCount = props.totalCount ?? 0
  if (readyCount <= 0) {
    return false
  }
  if (hasActiveDocuments.value || hasFailedDocuments.value) {
    return true
  }
  return readyCount < totalCount
})
const visibleStatCount = computed(() => {
  let count = showTotalStat.value ? 1 : 0
  if (showReadyStat.value) count += 1
  if (hasActiveDocuments.value) count += 1
  if (hasFailedDocuments.value) count += 1
  if (hasCostSummary.value) count += 1
  if (showAvgCostStat.value) count += 1
  return count
})
const showSoloStatLayout = computed(() => Boolean(props.hasDocuments) && visibleStatCount.value <= 1)
const compactOverview = computed(() => Boolean(props.hasDocuments) && visibleStatCount.value <= 2)

function uploadFailureKindLabel(failure: DocumentUploadFailure): string | null {
  if (!failure.rejectionKind) return null
  const key = `documents.uploadReport.rejectionKinds.${failure.rejectionKind}`
  return t(key) === key ? failure.rejectionKind : t(key)
}

function openUploader(): void {
  uploadRef.value?.openPicker()
}

function formatCost(amount: number, currencyCode = 'USD'): string {
  if (amount <= 0) return '—'
  if (amount < 0.01) {
    return currencyCode === 'USD' ? '<$0.01' : `<0.01 ${currencyCode}`
  }
  const formatted = new Intl.NumberFormat(undefined, {
    style: 'currency',
    currency: currencyCode,
    minimumFractionDigits: 2,
    maximumFractionDigits: 2,
  }).format(amount)

  return formatted.replace(/([.,]\d*?[1-9])0+(?=\D*$)/, '$1').replace(/([.,])0+(?=\D*$)/, '')
}

function formatAvgCost(total: number, count: number, currencyCode = 'USD'): string {
  if (count === 0) return '—'
  const avg = total / count
  return formatCost(avg, currencyCode)
}

defineExpose({ openUploader })
</script>

<template>
  <header
    class="rr-docs-header"
    :class="{
      'has-documents': Boolean(hasDocuments),
      'is-compact': compactOverview,
      'has-solo-stat': showSoloStatLayout,
    }"
  >
    <section class="rr-docs-header__overview">
      <div class="rr-docs-header__top">
        <div class="rr-docs-header__copy">
          <h1 class="rr-docs-header__title">{{ $t('documents.workspace.title') }}</h1>
          <p class="rr-docs-header__subtitle">{{ $t('documents.workspace.subtitle') }}</p>
        </div>
        <div class="rr-docs-header__actions">
          <UploadDropzone
            ref="uploadRef"
            :accepted-formats="acceptedFormats"
            :max-size-mb="maxSizeMb"
            :loading="loading"
            :has-documents="Boolean(hasDocuments)"
            @select="emit('select', $event)"
          />
        </div>
      </div>

      <div
        class="rr-docs-header__stats"
        :class="{ 'is-sparse': visibleStatCount <= 3, 'is-solo': showSoloStatLayout }"
      >
        <div
          v-if="showTotalStat"
          class="rr-docs-header__stat"
        >
          <span class="rr-docs-header__stat-value">{{ totalCount ?? 0 }}</span>
          <span class="rr-docs-header__stat-label">{{ $t('documents.workspace.stats.total') }}</span>
        </div>
        <div
          v-if="showReadyStat"
          class="rr-docs-header__stat rr-docs-header__stat--success"
        >
          <span class="rr-docs-header__stat-value">{{ readyCount ?? 0 }}</span>
          <span class="rr-docs-header__stat-label">{{ $t('documents.workspace.stats.ready') }}</span>
        </div>
        <div v-if="hasActiveDocuments" class="rr-docs-header__stat rr-docs-header__stat--warning">
          <span class="rr-docs-header__stat-value">{{ activeCount }}</span>
          <span class="rr-docs-header__stat-label">{{ $t('documents.workspace.stats.processing') }}</span>
        </div>
        <div v-if="hasFailedDocuments" class="rr-docs-header__stat rr-docs-header__stat--danger">
          <span class="rr-docs-header__stat-value">{{ failedCount }}</span>
          <span class="rr-docs-header__stat-label">{{ $t('documents.workspace.stats.failed') }}</span>
        </div>
        <div v-if="hasCostSummary" class="rr-docs-header__stat rr-docs-header__stat--cost">
          <span class="rr-docs-header__stat-value">{{ formatCost(costSummary.totalCost, costSummary.currencyCode) }}</span>
          <span class="rr-docs-header__stat-label">{{ $t('documents.workspace.stats.totalCost') }}</span>
        </div>
        <div
          v-if="showAvgCostStat"
          class="rr-docs-header__stat rr-docs-header__stat--avg-cost"
        >
          <span class="rr-docs-header__stat-value">{{ formatAvgCost(costSummary.totalCost, costSummary.documentCount, costSummary.currencyCode) }}</span>
          <span class="rr-docs-header__stat-label">{{ $t('documents.workspace.stats.avgCost') }}</span>
        </div>
      </div>
    </section>

    <section
      v-if="uploadFailures.length"
      class="rr-docs-header__alert"
      role="status"
      aria-live="polite"
    >
      <div class="rr-docs-header__alert-top">
        <div>
          <strong>{{ $t('documents.uploadReport.title') }}</strong>
          <p>{{ uploadFailureSummary }}</p>
        </div>
        <button type="button" class="rr-button rr-button--ghost rr-button--tiny" @click="emit('clearFailures')">
          {{ $t('documents.uploadReport.dismiss') }}
        </button>
      </div>
      <details>
        <summary>{{ $t('documents.uploadReport.showDetails') }}</summary>
        <ul class="rr-docs-header__alert-list">
          <li v-for="failure in uploadFailures" :key="`${failure.fileName}:${failure.message}`">
            <strong>{{ failure.fileName }}</strong>
            <span v-if="uploadFailureKindLabel(failure)" class="rr-docs-header__alert-kind">{{ uploadFailureKindLabel(failure) }}</span>
            <span>{{ failure.message }}</span>
          </li>
        </ul>
      </details>
    </section>
  </header>
</template>

<style scoped>
.rr-docs-header {
  display: grid;
  gap: 10px;
}

.rr-docs-header.has-documents {
  gap: 8px;
}

.rr-docs-header.is-compact {
  gap: 6px;
}

.rr-docs-header__overview {
  display: grid;
  gap: 8px;
  padding: 10px 13px;
  border: 1px solid rgba(226, 232, 240, 0.9);
  border-radius: 20px;
  background:
    radial-gradient(circle at top left, rgba(99, 102, 241, 0.08), transparent 34%),
    linear-gradient(180deg, rgba(255, 255, 255, 0.98), rgba(248, 250, 252, 0.96));
  box-shadow: 0 18px 42px rgba(15, 23, 42, 0.05);
}

.rr-docs-header.has-documents .rr-docs-header__overview {
  gap: 5px;
  padding: 8px 11px;
  border-radius: 18px;
}

.rr-docs-header.is-compact .rr-docs-header__overview {
  gap: 4px;
  padding: 6px 9px;
  border-radius: 16px;
}

.rr-docs-header.has-solo-stat .rr-docs-header__overview {
  gap: 4px;
}

.rr-docs-header.is-compact .rr-docs-header__top {
  gap: 7px;
}

.rr-docs-header__top {
  display: grid;
  grid-template-columns: minmax(0, 1fr);
  gap: 10px;
  align-items: center;
}

.rr-docs-header__copy {
  display: grid;
  gap: 4px;
}

.rr-docs-header.has-documents .rr-docs-header__copy {
  gap: 3px;
}

.rr-docs-header.is-compact .rr-docs-header__copy {
  gap: 2px;
}

.rr-docs-header.has-documents .rr-docs-header__subtitle {
  display: none;
}

.rr-docs-header__title {
  margin: 0;
  font-size: 1.28rem;
  font-weight: 700;
  letter-spacing: -0.03em;
  line-height: 1.08;
  color: var(--rr-text-primary, #0f172a);
}

.rr-docs-header.has-documents .rr-docs-header__title {
  font-size: 1.18rem;
}

.rr-docs-header.is-compact .rr-docs-header__title {
  font-size: 1.06rem;
}

.rr-docs-header__subtitle {
  margin: 0;
  max-width: 72ch;
  font-size: 0.81rem;
  color: var(--rr-text-muted, rgba(15, 23, 42, 0.55));
  line-height: 1.5;
}

.rr-docs-header.has-documents .rr-docs-header__subtitle {
  max-width: 64ch;
  font-size: 0.78rem;
  line-height: 1.4;
}

.rr-docs-header__stats {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(128px, 1fr));
  gap: 7px;
}

.rr-docs-header__stats.is-sparse {
  grid-template-columns: repeat(auto-fit, minmax(148px, 176px));
  justify-content: start;
}

.rr-docs-header__stats.is-solo {
  grid-template-columns: minmax(132px, 160px);
  max-width: 160px;
}

.rr-docs-header.has-documents .rr-docs-header__stats {
  gap: 6px;
}

.rr-docs-header.is-compact .rr-docs-header__stats {
  grid-template-columns: repeat(auto-fit, minmax(132px, 168px));
  gap: 5px;
}

.rr-docs-header__stat {
  display: grid;
  gap: 3px;
  padding: 9px 11px;
  border: 1px solid rgba(226, 232, 240, 0.92);
  border-radius: 16px;
  background: rgba(255, 255, 255, 0.94);
  box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.72);
  min-width: 0;
}

.rr-docs-header.has-documents .rr-docs-header__stat {
  gap: 2px;
  padding: 8px 10px;
  border-radius: 14px;
}

.rr-docs-header.is-compact .rr-docs-header__stat {
  gap: 1px;
  padding: 7px 9px;
  border-radius: 12px;
}

.rr-docs-header.has-solo-stat .rr-docs-header__stat {
  padding: 7px 9px;
  border-radius: 13px;
}

.rr-docs-header__stat-value {
  font-size: 1.1rem;
  font-weight: 700;
  letter-spacing: -0.02em;
  color: var(--rr-text-primary, #0f172a);
}

.rr-docs-header.has-documents .rr-docs-header__stat-value {
  font-size: 1.02rem;
}

.rr-docs-header.is-compact .rr-docs-header__stat-value {
  font-size: 0.98rem;
}

.rr-docs-header__stat-label {
  font-size: 0.65rem;
  font-weight: 600;
  text-transform: uppercase;
  letter-spacing: 0.04em;
  color: var(--rr-text-muted, rgba(15, 23, 42, 0.5));
}

.rr-docs-header.has-documents .rr-docs-header__stat-label {
  font-size: 0.62rem;
}

.rr-docs-header.is-compact .rr-docs-header__stat-label {
  font-size: 0.58rem;
}

.rr-docs-header__stat--success .rr-docs-header__stat-value { color: #059669; }
.rr-docs-header__stat--warning .rr-docs-header__stat-value { color: #d97706; }
.rr-docs-header__stat--danger .rr-docs-header__stat-value { color: #dc2626; }
.rr-docs-header__stat--cost .rr-docs-header__stat-value { color: #7c3aed; }

.rr-docs-header__alert {
  padding: 12px 16px;
  border-radius: 10px;
  border: 1px solid rgba(239, 68, 68, 0.15);
  background: rgba(254, 242, 242, 0.9);
}

.rr-docs-header__alert-top {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 12px;
}

.rr-docs-header__alert-top p { margin: 2px 0 0; color: rgba(127, 29, 29, 0.74); }

.rr-docs-header__alert-list {
  display: grid;
  gap: 6px;
  padding-left: 16px;
  margin: 8px 0 0;
  color: rgba(127, 29, 29, 0.84);
}

.rr-docs-header__alert-kind {
  padding: 2px 6px;
  border-radius: 999px;
  background: rgba(127, 29, 29, 0.08);
  font-size: 0.72rem;
  font-weight: 700;
}

@media (min-width: 900px) {
  .rr-docs-header.has-solo-stat .rr-docs-header__overview {
    width: min(100%, 58rem);
  }

  .rr-docs-header__top {
    grid-template-columns: minmax(0, 1fr) auto;
  }

  .rr-docs-header__actions {
    justify-self: end;
  }

  .rr-docs-header.is-compact .rr-docs-header__top {
    align-items: start;
  }
}

@media (max-width: 920px) {
  .rr-docs-header {
    gap: 9px;
  }

  .rr-docs-header__overview {
    gap: 9px;
    padding: 10px 12px;
    border-radius: 18px;
  }

  .rr-docs-header.has-documents .rr-docs-header__overview {
    gap: 5px;
    padding: 8px 10px;
  }

  .rr-docs-header.is-compact .rr-docs-header__overview {
    gap: 4px;
    padding: 7px 8px;
  }

  .rr-docs-header__top {
    gap: 10px;
  }

  .rr-docs-header__copy {
    gap: 4px;
  }

  .rr-docs-header__title {
    font-size: 1.34rem;
  }

  .rr-docs-header.has-documents .rr-docs-header__title {
    font-size: 1.18rem;
  }

  .rr-docs-header.is-compact .rr-docs-header__title {
    font-size: 1.04rem;
  }

  .rr-docs-header__subtitle {
    font-size: 0.84rem;
    line-height: 1.4;
  }

  .rr-docs-header.has-documents .rr-docs-header__subtitle {
    font-size: 0.76rem;
  }

  .rr-docs-header__stats {
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 8px;
  }

  .rr-docs-header__stats.is-sparse {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }

  .rr-docs-header__stats.is-solo {
    grid-template-columns: minmax(124px, 156px);
    max-width: 156px;
  }

  .rr-docs-header.is-compact .rr-docs-header__stats {
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 6px;
  }

  .rr-docs-header__stat {
    gap: 2px;
    padding: 9px 11px;
    border-radius: 14px;
  }

  .rr-docs-header__stat-value {
    font-size: 1.12rem;
  }

  .rr-docs-header.has-documents .rr-docs-header__stat-value {
    font-size: 1rem;
  }

  .rr-docs-header.is-compact .rr-docs-header__stat-value {
    font-size: 0.96rem;
  }

  .rr-docs-header__stat-label {
    font-size: 0.67rem;
  }

  .rr-docs-header.has-documents .rr-docs-header__stat-label {
    font-size: 0.62rem;
  }
}

@media (max-width: 600px) {
  .rr-docs-header.is-compact {
    gap: 6px;
  }

  .rr-docs-header.is-compact .rr-docs-header__overview {
    gap: 5px;
    padding: 6px 8px;
    border-radius: 14px;
  }

  .rr-docs-header.is-compact .rr-docs-header__top {
    gap: 6px;
  }

  .rr-docs-header.is-compact .rr-docs-header__stats {
    grid-template-columns: minmax(108px, 132px);
    justify-content: start;
  }

  .rr-docs-header.is-compact .rr-docs-header__stat {
    padding: 6px 8px;
    border-radius: 11px;
  }
}

@media (min-width: 1240px) {
  .rr-docs-header.has-solo-stat .rr-docs-header__overview {
    width: min(100%, 60rem);
  }

  .rr-docs-header__top {
    gap: 14px;
  }

  .rr-docs-header__stats {
    grid-template-columns: repeat(auto-fit, minmax(176px, 1fr));
  }

  .rr-docs-header__stats.is-sparse {
    grid-template-columns: repeat(auto-fit, minmax(164px, 188px));
  }

  .rr-docs-header__stats.is-solo {
    grid-template-columns: minmax(148px, 176px);
    max-width: 176px;
  }
}

@media (min-width: 1800px) {
  .rr-docs-header {
    gap: 12px;
  }

  .rr-docs-header__overview {
    gap: 10px;
    padding: 12px 14px;
  }

  .rr-docs-header__copy {
    gap: 4px;
    max-width: 84ch;
  }

  .rr-docs-header__title {
    font-size: 1.42rem;
  }

  .rr-docs-header__subtitle {
    font-size: 0.86rem;
  }

  .rr-docs-header__stats {
    grid-template-columns: repeat(6, minmax(0, 1fr));
    gap: 8px;
  }

  .rr-docs-header__stats.is-sparse {
    grid-template-columns: repeat(auto-fit, minmax(176px, 210px));
  }

  .rr-docs-header__stats.is-solo {
    grid-template-columns: minmax(156px, 184px);
    max-width: 184px;
  }

  .rr-docs-header__stat {
    gap: 3px;
    padding: 10px 12px;
    border-radius: 14px;
  }

  .rr-docs-header__stat-value {
    font-size: 1.18rem;
  }

  .rr-docs-header__stat-label {
    font-size: 0.68rem;
  }
}

@media (min-width: 2400px) {
  .rr-docs-header__top {
    gap: 18px;
  }

  .rr-docs-header__stats {
    grid-template-columns: repeat(6, minmax(0, 1fr));
    gap: 10px;
  }

  .rr-docs-header__stats.is-sparse {
    grid-template-columns: repeat(auto-fit, minmax(190px, 224px));
  }

  .rr-docs-header__stats.is-solo {
    grid-template-columns: minmax(164px, 192px);
    max-width: 192px;
  }
}

@media (max-width: 600px) {
  .rr-docs-header {
    gap: 6px;
  }

  .rr-docs-header__overview {
    gap: 7px;
    padding: 8px 9px;
    border-radius: 16px;
  }

  .rr-docs-header.is-compact .rr-docs-header__overview {
    gap: 5px;
    padding: 6px 7px;
    border-radius: 14px;
  }

  .rr-docs-header.has-documents .rr-docs-header__overview {
    gap: 3px;
    padding: 6px 7px;
    border-radius: 14px;
  }

  .rr-docs-header__top {
    grid-template-columns: 1fr;
    gap: 6px;
  }

  .rr-docs-header.has-documents .rr-docs-header__copy {
    gap: 1px;
  }

  .rr-docs-header.has-documents .rr-docs-header__subtitle {
    display: none;
  }

  .rr-docs-header__title {
    font-size: 1.24rem;
  }

  .rr-docs-header.has-documents .rr-docs-header__title {
    font-size: 1.02rem;
  }

  .rr-docs-header.is-compact .rr-docs-header__title {
    font-size: 0.98rem;
  }

  .rr-docs-header__stats {
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 5px;
  }

  .rr-docs-header__stats.is-sparse {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }

  .rr-docs-header__stats.is-solo {
    grid-template-columns: repeat(1, minmax(0, 1fr));
    max-width: none;
  }

  .rr-docs-header.has-documents .rr-docs-header__stat {
    padding: 6px 8px;
    border-radius: 11px;
  }

  .rr-docs-header.is-compact .rr-docs-header__stat {
    padding: 5px 7px;
    border-radius: 10px;
  }

  .rr-docs-header.has-documents .rr-docs-header__stat-value {
    font-size: 0.92rem;
  }

  .rr-docs-header.is-compact .rr-docs-header__stat-value {
    font-size: 0.88rem;
  }

  .rr-docs-header.has-documents .rr-docs-header__stat-label {
    font-size: 0.56rem;
  }

  .rr-docs-header.has-documents .rr-docs-header__stat--avg-cost {
    display: none;
  }
}
</style>
