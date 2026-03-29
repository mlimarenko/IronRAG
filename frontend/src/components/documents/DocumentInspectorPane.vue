<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import StatusPill from 'src/components/base/StatusPill.vue'
import { useDisplayFormatters } from 'src/composables/useDisplayFormatters'
import type { DocumentDetail, DocumentStatus } from 'src/models/ui/documents'

const props = defineProps<{
  open: boolean
  detail: DocumentDetail | null
  loading: boolean
  error: string | null
  downloadingId?: string | null
}>()

const emit = defineEmits<{
  close: []
  append: [id: string]
  replace: [id: string]
  retry: [id: string]
  remove: [id: string]
  openInGraph: [graphNodeId: string]
  downloadText: [id: string]
}>()

const { t } = useI18n()
const {
  documentMetadataLabel,
  documentStatusLabel,
  mutationKindLabel,
  formatDateTime,
  uploadFailureLabel,
} =
  useDisplayFormatters()
const previewExpanded = ref(false)
const previewCollapseThreshold = 560

const statusLabel = computed(() =>
  props.detail ? documentStatusLabel(props.detail.status) : t('documents.statuses.queued'),
)

const mutationLabel = computed(() => {
  const detail = props.detail
  if (!detail?.mutation.kind) {
    return null
  }
  const kindLabel = mutationKindLabel(detail.mutation.kind)
  const statusKey = detail.mutation.status
    ? `documents.mutation.status.${detail.mutation.status}`
    : null
  const statusLabelValue = statusKey && t(statusKey)
  return statusLabelValue ? `${kindLabel} · ${statusLabelValue}` : kindLabel
})

function mutationTone(status: string | null): DocumentStatus {
  if (status === 'failed') {
    return 'failed'
  }
  if (status === 'accepted' || status === 'reconciling') {
    return 'processing'
  }
  return 'ready'
}

const metaLine = computed(() => {
  if (!props.detail) {
    return ''
  }
  return [props.detail.fileType, props.detail.fileSizeLabel, formatDateTime(props.detail.uploadedAt)]
    .filter(Boolean)
    .join(' · ')
})

const previewText = computed(() => {
  const preview = props.detail?.extractedStats.previewText?.trim() ?? ''
  return preview.length > 0 ? preview : null
})

const previewIsLong = computed(
  () => (previewText.value?.length ?? 0) > previewCollapseThreshold,
)

const previewVisibleText = computed(() => {
  if (!previewText.value) {
    return null
  }
  if (previewExpanded.value || !previewIsLong.value) {
    return previewText.value
  }
  return `${previewText.value.slice(0, previewCollapseThreshold).trimEnd()}…`
})

const summaryLine = computed(() => {
  const summary = props.detail?.summary?.trim() ?? ''
  if (!summary.length) {
    return null
  }
  const fileName = props.detail?.fileName?.trim() ?? ''
  return summary === fileName ? null : summary
})

const mutationSummary = computed(() => mutationLabel.value)

const mutationWarningLabel = computed(() => uploadFailureLabel(props.detail?.mutation.warning ?? null))

const hasQuickExploreActions = computed(() =>
  Boolean(props.detail?.graphNodeId || (previewText.value && props.detail?.canDownloadText)),
)

watch(
  () => props.detail?.id ?? null,
  () => {
    previewExpanded.value = false
  },
)

function formatCost(amount: number | null, currencyCode: string | null): string {
  if (amount === null || amount <= 0) {
    return '—'
  }
  if (amount < 0.001) {
    return currencyCode === 'USD' || !currencyCode ? '<$0.001' : `<0.001 ${currencyCode}`
  }
  const formatter = new Intl.NumberFormat(undefined, {
    style: 'currency',
    currency: currencyCode ?? 'USD',
    minimumFractionDigits: amount < 0.01 ? 4 : 2,
    maximumFractionDigits: amount < 0.01 ? 4 : 3,
  })
  return formatter.format(amount)
}

const overviewRows = computed(() => {
  const detail = props.detail
  if (!detail) {
    return []
  }
  return [
    { key: 'uploaded', label: documentMetadataLabel('uploaded'), value: formatDateTime(detail.uploadedAt) },
    { key: 'status', label: documentMetadataLabel('status'), value: documentStatusLabel(detail.status) },
    detail.activeRevisionNo
      ? {
          key: 'revision',
          label: documentMetadataLabel('activeRevision'),
          value: `#${String(detail.activeRevisionNo)}`,
        }
      : null,
    detail.extractedStats.chunkCount !== null
      ? {
          key: 'chunks',
          label: documentMetadataLabel('chunkCount'),
          value: String(detail.extractedStats.chunkCount),
        }
      : null,
    detail.totalEstimatedCost !== null
      ? {
          key: 'totalCost',
          label: documentMetadataLabel('totalCost'),
          value: formatCost(detail.totalEstimatedCost, detail.currency),
        }
      : null,
    detail.providerCallCount > 0
      ? {
          key: 'providerCalls',
          label: documentMetadataLabel('providerCalls'),
          value: String(detail.providerCallCount),
        }
      : null,
  ].filter((item): item is { key: string; label: string; value: string } => item !== null)
})
</script>

<template>
  <aside
    v-if="props.open"
    class="rr-document-inspector"
  >
    <header class="rr-document-inspector__header">
      <div class="rr-document-inspector__header-main">
        <div class="rr-document-inspector__copy">
          <span class="rr-document-inspector__eyebrow">{{ $t('documents.headers.document') }}</span>
          <h2 v-if="props.detail">{{ props.detail.fileName }}</h2>
          <h2 v-else>{{ $t('documents.details.title') }}</h2>
          <p v-if="props.detail">{{ metaLine }}</p>
        </div>

        <button
          type="button"
          class="rr-button rr-button--ghost rr-button--tiny"
          @click="emit('close')"
        >
          {{ $t('dialogs.close') }}
        </button>
      </div>

      <div
        v-if="props.detail"
        class="rr-document-inspector__summary-strip"
      >
        <div class="rr-document-inspector__summary-strip-main">
          <StatusPill
            :tone="props.detail.status"
            :label="statusLabel"
          />
        </div>
        <div
          v-if="hasQuickExploreActions"
          class="rr-document-inspector__summary-strip-actions"
        >
          <button
            v-if="props.detail.graphNodeId"
            class="rr-button rr-button--ghost rr-button--tiny"
            type="button"
            @click="emit('openInGraph', props.detail.graphNodeId)"
          >
            {{ $t('documents.details.openInGraph') }}
          </button>
          <button
            v-if="previewText && props.detail.canDownloadText"
            class="rr-button rr-button--ghost rr-button--tiny"
            type="button"
            :disabled="props.downloadingId === props.detail.id"
            @click="emit('downloadText', props.detail.id)"
          >
            {{ props.downloadingId === props.detail.id ? '…' : $t('documents.details.downloadText') }}
          </button>
        </div>
      </div>
    </header>

    <div
      v-if="props.loading"
      class="rr-document-inspector__empty"
    >
      {{ $t('documents.loadingDetail') }}
    </div>

    <div
      v-else-if="props.error"
      class="rr-document-inspector__empty"
    >
      {{ props.error }}
    </div>

    <template v-else-if="props.detail">
      <p
        v-if="summaryLine"
        class="rr-document-inspector__lead"
      >
        {{ summaryLine }}
      </p>

      <section class="rr-document-inspector__section">
        <div class="rr-document-inspector__section-head">
          <strong>{{ $t('documents.details.keyInfo') }}</strong>
        </div>
        <div class="rr-document-inspector__fact-grid">
          <article
            v-for="item in overviewRows"
            :key="item.key"
            class="rr-document-inspector__fact-card"
          >
            <span class="rr-document-inspector__fact-label">{{ item.label }}</span>
            <strong class="rr-document-inspector__fact-value">{{ item.value }}</strong>
          </article>
        </div>
        <div
          v-if="mutationSummary || mutationWarningLabel"
          class="rr-document-inspector__activity"
        >
          <div
            v-if="mutationSummary"
            class="rr-document-inspector__activity-row"
          >
            <span class="rr-document-inspector__activity-label">{{ $t('documents.details.latestChange') }}</span>
            <strong class="rr-document-inspector__activity-value">{{ mutationSummary }}</strong>
          </div>
          <p
            v-if="mutationWarningLabel"
            class="rr-document-inspector__microcopy"
          >
            {{ mutationWarningLabel }}
          </p>
        </div>
      </section>

      <section class="rr-document-inspector__section">
        <div class="rr-document-inspector__section-head">
          <strong>{{ $t('documents.details.readableText') }}</strong>
          <button
            v-if="previewText && previewIsLong"
            type="button"
            class="rr-document-inspector__link-button"
            @click="previewExpanded = !previewExpanded"
          >
            {{ previewExpanded ? $t('documents.details.showLess') : $t('documents.details.showMore') }}
          </button>
        </div>
        <p
          v-if="previewText"
          class="rr-document-inspector__preview"
        >
          {{ previewVisibleText }}
        </p>
        <p
          v-else
          class="rr-document-inspector__microcopy"
        >
          {{ $t('documents.details.notReadableYet') }}
        </p>
        <p
          v-if="previewText && previewIsLong && !previewExpanded"
          class="rr-document-inspector__microcopy"
        >
          {{ $t('documents.details.previewTruncated') }}
        </p>
      </section>

      <section class="rr-document-inspector__section rr-document-inspector__actions">
        <div class="rr-document-inspector__action-group">
          <span class="rr-document-inspector__action-label">{{ $t('documents.actions.groups.update') }}</span>
          <div class="rr-document-inspector__action-row">
            <button
              v-if="props.detail.canAppend"
              class="rr-button rr-button--secondary rr-button--tiny"
              type="button"
              @click="emit('append', props.detail.id)"
            >
              {{ $t('documents.actions.append') }}
            </button>
            <button
              v-if="props.detail.canReplace"
              class="rr-button rr-button--secondary rr-button--tiny"
              type="button"
              @click="emit('replace', props.detail.id)"
            >
              {{ $t('documents.actions.replace') }}
            </button>
          </div>
        </div>

        <div class="rr-document-inspector__action-group">
          <span class="rr-document-inspector__action-label">{{ $t('documents.actions.groups.recovery') }}</span>
          <div class="rr-document-inspector__action-row">
            <button
              v-if="props.detail.canRetry"
              class="rr-button rr-button--ghost rr-button--tiny"
              type="button"
              @click="emit('retry', props.detail.id)"
            >
              {{ $t('documents.actions.retry') }}
            </button>
            <button
              v-if="props.detail.canRemove"
              class="rr-button rr-button--ghost rr-button--tiny is-danger"
              type="button"
              @click="emit('remove', props.detail.id)"
            >
              {{ $t('documents.actions.remove') }}
            </button>
          </div>
        </div>
      </section>
    </template>

    <div
      v-else
      class="rr-document-inspector__empty"
    >
      {{ $t('documents.details.empty') }}
    </div>
  </aside>
</template>

<style scoped lang="scss">
.rr-document-inspector {
  display: grid;
  gap: 0.92rem;
  padding: 1rem;
  border: 1px solid rgba(15, 23, 42, 0.07);
  border-radius: 1.25rem;
  background: rgba(255, 255, 255, 0.96);
  box-shadow: 0 18px 34px rgba(15, 23, 42, 0.05);
}

.rr-document-inspector__header,
.rr-document-inspector__section,
.rr-document-inspector__actions {
  display: grid;
  gap: 0.58rem;
}

.rr-document-inspector__header-main {
  display: flex;
  align-items: start;
  justify-content: space-between;
  gap: 0.75rem;
}

.rr-document-inspector__copy {
  display: grid;
  gap: 0.35rem;
}

.rr-document-inspector__copy h2 {
  margin: 0;
  font-size: 1.52rem;
  line-height: 1.04;
  letter-spacing: -0.035em;
  word-break: break-word;
  overflow-wrap: anywhere;
}

.rr-document-inspector__copy p,
.rr-document-inspector__microcopy,
.rr-document-inspector__preview,
.rr-document-inspector__lead,
.rr-document-inspector__empty {
  margin: 0;
}

.rr-document-inspector__eyebrow {
  color: rgba(15, 23, 42, 0.5);
  font-size: 0.78rem;
  font-weight: 700;
  letter-spacing: 0.08em;
  text-transform: uppercase;
}

.rr-document-inspector__summary-strip {
  display: flex;
  flex-wrap: wrap;
  align-items: flex-start;
  justify-content: space-between;
  gap: 0.6rem 0.75rem;
}

.rr-document-inspector__summary-strip-main,
.rr-document-inspector__summary-strip-actions {
  display: flex;
  flex-wrap: wrap;
  gap: 0.45rem;
}

.rr-document-inspector__lead {
  padding: 0.72rem 0.82rem;
  border-radius: 0.9rem;
  background: rgba(247, 249, 252, 0.92);
  color: rgba(15, 23, 42, 0.72);
  font-size: 0.9rem;
  line-height: 1.5;
}

.rr-document-inspector__section {
  padding-top: 0.74rem;
  border-top: 1px solid rgba(15, 23, 42, 0.07);
}

.rr-document-inspector__section-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 0.75rem;
}

.rr-document-inspector__preview {
  max-height: 18rem;
  overflow: auto;
  padding: 0.78rem 0.84rem;
  border-radius: 0.9rem;
  background: rgba(247, 249, 252, 0.92);
  color: rgba(15, 23, 42, 0.82);
  font-size: 0.89rem;
  line-height: 1.58;
  white-space: pre-wrap;
}

.rr-document-inspector__microcopy {
  color: rgba(15, 23, 42, 0.56);
  font-size: 0.9rem;
  line-height: 1.45;
}

.rr-document-inspector__link-button {
  border: 0;
  padding: 0;
  background: transparent;
  color: rgba(59, 130, 246, 0.9);
  font: inherit;
  font-size: 0.84rem;
  font-weight: 600;
  cursor: pointer;
}

.rr-document-inspector__link-button:hover {
  color: rgba(37, 99, 235, 0.96);
}

.rr-document-inspector__fact-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 0.58rem;
}

.rr-document-inspector__fact-card {
  display: grid;
  gap: 0.28rem;
  padding: 0.68rem 0.74rem;
  border: 1px solid rgba(226, 232, 240, 0.86);
  border-radius: 0.9rem;
  background: rgba(255, 255, 255, 0.92);
}

.rr-document-inspector__fact-label {
  font-size: 0.72rem;
  font-weight: 700;
  letter-spacing: 0.06em;
  text-transform: uppercase;
  color: rgba(15, 23, 42, 0.46);
}

.rr-document-inspector__fact-value {
  color: rgba(15, 23, 42, 0.9);
  font-size: 0.92rem;
  line-height: 1.4;
  font-variant-numeric: tabular-nums;
}

.rr-document-inspector__activity {
  display: grid;
  gap: 0.48rem;
  padding: 0.72rem 0.82rem;
  border-radius: 0.9rem;
  background: rgba(245, 247, 255, 0.92);
}

.rr-document-inspector__activity-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 0.75rem;
  flex-wrap: wrap;
}

.rr-document-inspector__activity-label {
  font-size: 0.78rem;
  font-weight: 700;
  letter-spacing: 0.08em;
  text-transform: uppercase;
  color: rgba(15, 23, 42, 0.48);
}

.rr-document-inspector__activity-value {
  color: rgba(15, 23, 42, 0.86);
  font-size: 0.88rem;
  font-weight: 700;
  line-height: 1.4;
}

.rr-document-inspector__actions {
  gap: 0.74rem;
  padding-top: 0.74rem;
  border-top: 1px solid rgba(15, 23, 42, 0.07);
}

.rr-document-inspector__action-group {
  display: grid;
  gap: 0.48rem;
  padding: 0.72rem 0.82rem;
  border: 1px solid rgba(226, 232, 240, 0.82);
  border-radius: 0.95rem;
  background: rgba(255, 255, 255, 0.92);
}

.rr-document-inspector__action-label {
  font-size: 0.78rem;
  font-weight: 700;
  letter-spacing: 0.08em;
  text-transform: uppercase;
  color: rgba(15, 23, 42, 0.48);
}

.rr-document-inspector__action-row {
  display: flex;
  flex-wrap: wrap;
  gap: 0.5rem;
}

.rr-document-inspector__empty {
  display: grid;
  place-items: center;
  min-height: 10rem;
  color: rgba(15, 23, 42, 0.56);
}

@media (min-width: 1280px) {
  .rr-document-inspector__fact-grid {
    grid-template-columns: repeat(3, minmax(0, 1fr));
  }

  .rr-document-inspector__actions {
    grid-template-columns: repeat(2, minmax(0, 1fr));
    align-items: start;
  }

  .rr-document-inspector__action-group {
    height: 100%;
  }
}

@media (min-width: 1025px) {
  .rr-document-inspector {
    position: sticky;
    top: 1rem;
    max-height: calc(100vh - 7rem);
    overflow: auto;
  }
}

@media (min-width: 1800px) {
  .rr-document-inspector {
    gap: 0.9rem;
    padding: 1rem;
  }

  .rr-document-inspector__header,
  .rr-document-inspector__section,
  .rr-document-inspector__actions {
    gap: 0.6rem;
  }

  .rr-document-inspector__summary-strip {
    align-items: center;
  }

  .rr-document-inspector__fact-grid {
    grid-template-columns: repeat(3, minmax(0, 1fr));
  }
}

@media (max-width: 820px) {
  .rr-document-inspector {
    max-height: min(72vh, 44rem);
    overflow: auto;
  }
}

@media (max-width: 720px) {
  .rr-document-inspector {
    gap: 0.85rem;
    padding: 1rem;
    border-radius: 1rem;
  }

  .rr-document-inspector__header-main {
    align-items: flex-start;
  }

  .rr-document-inspector__summary-strip {
    align-items: stretch;
    flex-direction: column;
  }
}

@media (max-width: 640px) {
  .rr-document-inspector__fact-grid {
    grid-template-columns: 1fr;
  }

  .rr-document-inspector__activity-row {
    align-items: flex-start;
  }
}
</style>
