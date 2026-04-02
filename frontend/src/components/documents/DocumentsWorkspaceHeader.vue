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

  if ((props.activeCount ?? 0) > 0) {
    items.push({
      key: 'processing',
      label: t('documents.workspace.stats.processing'),
      value: props.activeCount ?? 0,
      tone: 'warning',
    })
  }
  if ((props.failedCount ?? 0) > 0) {
    items.push({
      key: 'failed',
      label: t('documents.workspace.stats.failed'),
      value: props.failedCount ?? 0,
      tone: 'danger',
    })
  }

  return items
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
  gap: 6px;
}

.rr-docs-header__overview {
  display: grid;
  gap: 6px;
  padding: 4px 2px 2px;
}

.rr-docs-header__topline {
  display: grid;
  grid-template-columns: minmax(0, 1fr) auto;
  gap: 12px 16px;
  align-items: start;
}

.rr-docs-header__copy {
  display: grid;
  gap: 0.28rem;
  min-width: 0;
}

.rr-docs-header__title {
  margin: 0;
  font-size: 1.02rem;
  font-weight: 700;
  letter-spacing: -0.03em;
  line-height: 1.12;
  color: var(--rr-text-primary, #0f172a);
}

.rr-docs-header__subtitle,
.rr-docs-header__context {
  margin: 0;
  font-size: 0.74rem;
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
  gap: 0.42rem;
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
  gap: 0.38rem;
  align-content: start;
  min-width: 0;
}

.rr-docs-header__stat-chip {
  display: inline-flex;
  align-items: center;
  gap: 0.34rem;
  min-height: 1.9rem;
  padding: 0.28rem 0.62rem;
  border: 1px solid rgba(226, 232, 240, 0.9);
  border-radius: 999px;
  background: rgba(248, 250, 252, 0.9);
}

.rr-docs-header__stat-chip strong {
  font-size: 0.86rem;
  font-weight: 700;
  line-height: 1;
  color: var(--rr-text-primary, #0f172a);
}

.rr-docs-header__stat-chip span {
  font-size: 0.66rem;
  font-weight: 600;
  line-height: 1;
  letter-spacing: 0.04em;
  text-transform: uppercase;
  color: rgba(71, 85, 105, 0.8);
}

.rr-docs-header__stat-chip--success strong {
  color: #059669;
}

.rr-docs-header__stat-chip--info strong {
  color: #0f766e;
}

.rr-docs-header__stat-chip--warning strong {
  color: #d97706;
}

.rr-docs-header__stat-chip--danger strong {
  color: #dc2626;
}

.rr-docs-header__stat-chip--cost strong {
  color: #7c3aed;
}

.rr-docs-header__alert {
  padding: 10px 12px;
  border-radius: 14px;
  border: 1px solid rgba(239, 68, 68, 0.16);
  background: rgba(254, 242, 242, 0.84);
}

.rr-docs-header__alert-top {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 12px;
}

.rr-docs-header__alert-top p {
  margin: 2px 0 0;
  color: rgba(127, 29, 29, 0.74);
}

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

@media (max-width: 920px) {
  .rr-docs-header__overview {
    padding: 2px 0 0;
  }

  .rr-docs-header__topline {
    grid-template-columns: minmax(0, 1fr);
    gap: 10px;
    align-items: start;
  }

  .rr-docs-header__actions {
    justify-self: stretch;
    justify-content: flex-start;
  }
}

@media (max-width: 600px) {
  .rr-docs-header__title {
    font-size: 1.04rem;
  }

  .rr-docs-header__stats {
    display: none;
  }

  .rr-docs-header__actions {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 0.44rem;
  }

  .rr-docs-header__actions :deep(.rr-button--tiny),
  .rr-docs-header__actions :deep(.rr-upload-dropzone) {
    width: 100%;
  }
}
</style>
