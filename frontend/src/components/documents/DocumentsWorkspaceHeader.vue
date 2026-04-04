<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import type { DocumentUploadFailure, LibraryCostSummary } from 'src/models/ui/documents'
import UploadDropzone from './UploadDropzone.vue'

const props = defineProps<{
  acceptedFormats: string[]
  maxSizeMb: number
  loading: boolean
  totalCount?: number
  activeCount?: number
  readableCount?: number
  failedCount?: number
  graphReadyCount?: number
  graphSparseCount?: number
  costSummary?: LibraryCostSummary | null
  uploadFailures: DocumentUploadFailure[]
  hasDocuments?: boolean
}>()

const emit = defineEmits<{
  select: [files: File[]]
  clearFailures: []
  openAddLink: []
}>()

const { t } = useI18n()

const uploadFailureSummary = computed(() => {
  const count = props.uploadFailures.length
  if (count === 0) return null
  return t('documents.uploadReport.summary', { count })
})

const contextCopy = computed(() => {
  const active = props.activeCount ?? 0
  if (!props.hasDocuments) {
    return t('documents.workspace.contextEmpty')
  }
  if (active > 0) {
    const total = props.totalCount ?? 0
    return t('documents.workspace.contextActive', { total, active })
  }
  return ''
})

const summaryItems = computed(() => {
  const items: { key: string; label: string; value: string | number; tone: string }[] = []
  const totalCount = props.totalCount ?? 0
  const graphReadyCount = props.graphReadyCount ?? 0
  const graphSparseCount = props.graphSparseCount ?? 0
  const activeCount = props.activeCount ?? 0
  const failedCount = props.failedCount ?? 0

  if (props.hasDocuments && totalCount > 0) {
    items.push({
      key: 'total',
      label: t('documents.workspace.stats.total'),
      value: totalCount,
      tone: 'default',
    })
  }

  if (props.hasDocuments && graphReadyCount > 0) {
    items.push({
      key: 'graphReady',
      label: t('documents.workspace.stats.graphReady'),
      value: graphReadyCount,
      tone: 'success',
    })
  }

  if (graphSparseCount > 0) {
    items.push({
      key: 'graphSparse',
      label: t('documents.workspace.stats.graphSparse'),
      value: graphSparseCount,
      tone: activeCount > 0 ? 'info' : 'warning',
    })
  }

  if (activeCount > 0) {
    items.push({
      key: 'processing',
      label: t('documents.workspace.stats.processing'),
      value: activeCount,
      tone: 'warning',
    })
  }

  if (failedCount > 0) {
    items.push({
      key: 'failed',
      label: t('documents.workspace.stats.failed'),
      value: failedCount,
      tone: 'danger',
    })
  }

  return items.slice(0, 4)
})

function uploadFailureKindLabel(failure: DocumentUploadFailure): string | null {
  if (!failure.rejectionKind) return null
  const key = `documents.uploadReport.rejectionKinds.${failure.rejectionKind}`
  return t(key) === key ? failure.rejectionKind : t(key)
}
</script>

<template>
  <header class="rr-docs-header">
    <section class="rr-docs-header__overview">
      <div class="rr-docs-header__topline">
        <div class="rr-docs-header__copy">
          <h1 class="rr-docs-header__title">{{ $t('documents.workspace.title') }}</h1>
          <p v-if="!hasDocuments" class="rr-docs-header__subtitle">
            {{ $t('documents.workspace.subtitle') }}
          </p>
          <p v-if="contextCopy.length > 0" class="rr-docs-header__context">{{ contextCopy }}</p>
        </div>

        <div class="rr-docs-header__actions">
          <button
            type="button"
            class="rr-button rr-button--secondary rr-button--tiny"
            @click="emit('openAddLink')"
          >
            {{ $t('documents.actions.addLink') }}
          </button>
          <UploadDropzone
            :accepted-formats="acceptedFormats"
            :max-size-mb="maxSizeMb"
            :loading="loading"
            variant="inline"
            :show-meta="false"
            @select="emit('select', $event)"
          />
        </div>
      </div>

      <div v-if="summaryItems.length" class="rr-docs-header__stats" role="list">
        <span
          v-for="item in summaryItems"
          :key="item.key"
          class="rr-docs-header__stat-chip"
          :class="`rr-docs-header__stat-chip--${item.tone}`"
          role="listitem"
        >
          <strong>{{ item.value }}</strong>
          <span>{{ item.label }}</span>
        </span>
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
        <button
          type="button"
          class="rr-button rr-button--ghost rr-button--tiny"
          @click="emit('clearFailures')"
        >
          {{ $t('documents.uploadReport.dismiss') }}
        </button>
      </div>
      <details>
        <summary>{{ $t('documents.uploadReport.showDetails') }}</summary>
        <ul class="rr-docs-header__alert-list">
          <li v-for="failure in uploadFailures" :key="`${failure.fileName}:${failure.message}`">
            <strong>{{ failure.fileName }}</strong>
            <span v-if="uploadFailureKindLabel(failure)" class="rr-docs-header__alert-kind">
              {{ uploadFailureKindLabel(failure) }}
            </span>
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

.rr-docs-header__overview {
  display: grid;
  gap: 10px;
  padding: 2px 2px 4px;
}

.rr-docs-header__topline {
  display: grid;
  grid-template-columns: minmax(0, 1fr) auto;
  gap: 12px 18px;
  align-items: start;
}

.rr-docs-header__copy {
  display: grid;
  gap: 0.35rem;
  min-width: 0;
}

.rr-docs-header__title {
  margin: 0;
  font-size: clamp(1.08rem, 0.96rem + 0.34vw, 1.34rem);
  font-weight: 700;
  letter-spacing: -0.03em;
  line-height: 1.06;
  color: var(--rr-text-primary, #0f172a);
}

.rr-docs-header__subtitle,
.rr-docs-header__context {
  margin: 0;
  font-size: 0.78rem;
  line-height: 1.5;
}

.rr-docs-header__subtitle {
  max-width: 70ch;
  color: var(--rr-text-muted, rgba(15, 23, 42, 0.55));
}

.rr-docs-header__context {
  max-width: 72ch;
  color: rgba(51, 65, 85, 0.88);
  font-weight: 500;
}

.rr-docs-header__actions {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  justify-content: flex-end;
  justify-self: end;
  gap: 0.5rem;
  max-width: none;
  min-width: 0;
}

.rr-docs-header__actions :deep(.rr-button--tiny) {
  min-height: 32px;
}

.rr-docs-header__actions :deep(.rr-button) {
  justify-content: center;
}

.rr-docs-header__actions :deep(.rr-upload-dropzone) {
  width: auto;
  min-width: 0;
}

.rr-docs-header__stats {
  display: flex;
  flex-wrap: wrap;
  gap: 0.5rem;
  min-width: 0;
}

.rr-docs-header__stat-chip {
  display: inline-grid;
  grid-auto-flow: row;
  gap: 0.15rem;
  min-width: 7.6rem;
  min-height: 0;
  padding: 0.62rem 0.78rem;
  border: 1px solid rgba(226, 232, 240, 0.86);
  border-radius: 1rem;
  background:
    linear-gradient(180deg, rgba(255, 255, 255, 0.98), rgba(248, 250, 252, 0.92)),
    rgba(248, 250, 252, 0.9);
  box-shadow:
    0 8px 18px rgba(15, 23, 42, 0.03),
    inset 0 1px 0 rgba(255, 255, 255, 0.82);
}

.rr-docs-header__stat-chip strong {
  font-size: 0.98rem;
  font-weight: 700;
  line-height: 1.08;
  color: var(--rr-text-primary, #0f172a);
}

.rr-docs-header__stat-chip span {
  color: rgba(71, 85, 105, 0.88);
  font-size: 0.68rem;
  font-weight: 700;
  line-height: 1.25;
  letter-spacing: 0.06em;
  text-transform: uppercase;
}

.rr-docs-header__stat-chip--success {
  border-color: rgba(167, 243, 208, 0.84);
  background:
    linear-gradient(180deg, rgba(240, 253, 244, 0.98), rgba(236, 253, 245, 0.94)),
    rgba(240, 253, 244, 0.92);
}

.rr-docs-header__stat-chip--success strong {
  color: #059669;
}

.rr-docs-header__stat-chip--info {
  border-color: rgba(125, 211, 252, 0.56);
  background:
    linear-gradient(180deg, rgba(240, 249, 255, 0.98), rgba(236, 254, 255, 0.94)),
    rgba(240, 249, 255, 0.92);
}

.rr-docs-header__stat-chip--info strong {
  color: #0f766e;
}

.rr-docs-header__stat-chip--warning {
  border-color: rgba(253, 224, 71, 0.5);
  background:
    linear-gradient(180deg, rgba(255, 251, 235, 0.98), rgba(255, 247, 237, 0.95)),
    rgba(255, 251, 235, 0.92);
}

.rr-docs-header__stat-chip--warning strong {
  color: #d97706;
}

.rr-docs-header__stat-chip--danger {
  border-color: rgba(252, 165, 165, 0.52);
  background:
    linear-gradient(180deg, rgba(254, 242, 242, 0.98), rgba(255, 241, 242, 0.95)),
    rgba(254, 242, 242, 0.92);
}

.rr-docs-header__stat-chip--danger strong {
  color: #dc2626;
}

.rr-docs-header__alert {
  display: grid;
  gap: 0.75rem;
  padding: 0.88rem 0.95rem;
  border-radius: 1rem;
  border: 1px solid rgba(252, 165, 165, 0.42);
  background:
    linear-gradient(180deg, rgba(254, 242, 242, 0.98), rgba(255, 247, 237, 0.94)),
    rgba(254, 242, 242, 0.92);
  box-shadow: 0 12px 24px rgba(127, 29, 29, 0.05);
}

.rr-docs-header__alert-top {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 0.85rem;
}

.rr-docs-header__alert-top strong {
  color: rgba(127, 29, 29, 0.94);
}

.rr-docs-header__alert-top p {
  margin: 0.2rem 0 0;
  color: rgba(153, 27, 27, 0.82);
  font-size: 0.76rem;
  line-height: 1.45;
}

.rr-docs-header__alert-list {
  display: grid;
  gap: 0.55rem;
  margin: 0.2rem 0 0;
  padding-left: 1rem;
}

.rr-docs-header__alert-list li {
  display: grid;
  gap: 0.16rem;
  color: rgba(71, 85, 105, 0.92);
  font-size: 0.74rem;
  line-height: 1.4;
}

.rr-docs-header__alert-kind {
  color: rgba(190, 24, 93, 0.86);
  font-weight: 700;
}

@media (max-width: 820px) {
  .rr-docs-header__topline {
    grid-template-columns: minmax(0, 1fr);
  }

  .rr-docs-header__actions {
    justify-self: stretch;
    justify-content: flex-start;
  }

  .rr-docs-header__stats {
    gap: 0.45rem;
  }

  .rr-docs-header__stat-chip {
    min-width: 7rem;
    padding: 0.58rem 0.7rem;
  }
}

@media (max-width: 640px) {
  .rr-docs-header__actions {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 0.5rem;
  }

  .rr-docs-header__stats {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }

  .rr-docs-header__alert-top {
    flex-direction: column;
  }

  .rr-docs-header__actions :deep(.rr-button--tiny),
  .rr-docs-header__actions :deep(.rr-upload-dropzone) {
    width: 100%;
  }
}
</style>
