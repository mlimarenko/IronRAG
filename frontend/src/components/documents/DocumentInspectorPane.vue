<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import StatusPill from 'src/components/base/StatusPill.vue'
import { useDisplayFormatters } from 'src/composables/useDisplayFormatters'
import type { DocumentDetail, DocumentStatus } from 'src/models/ui/documents'

const props = defineProps<{
  open: boolean
  detail: DocumentDetail | null
  loading: boolean
  error: string | null
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
const { documentMetadataLabel, documentStatusLabel, mutationKindLabel, formatDateTime } =
  useDisplayFormatters()

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

const summaryLine = computed(() => {
  const summary = props.detail?.summary?.trim() ?? ''
  return summary.length > 0 ? summary : null
})

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
        class="rr-document-inspector__status-row"
      >
        <StatusPill
          :tone="props.detail.status"
          :label="statusLabel"
        />
        <StatusPill
          v-if="props.detail.mutation.status"
          :tone="mutationTone(props.detail.mutation.status)"
          :label="mutationLabel ?? props.detail.mutation.status"
        />
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
          <strong>{{ $t('documents.details.readableText') }}</strong>
        </div>
        <p
          v-if="previewText"
          class="rr-document-inspector__preview"
        >
          {{ previewText }}
        </p>
        <p
          v-else
          class="rr-document-inspector__microcopy"
        >
          {{ $t('documents.details.notReadableYet') }}
        </p>
      </section>

      <section class="rr-document-inspector__section">
        <div class="rr-document-inspector__section-head">
          <strong>{{ $t('documents.details.keyInfo') }}</strong>
        </div>
        <dl class="rr-document-inspector__facts">
          <template
            v-for="item in overviewRows"
            :key="item.key"
          >
            <dt>{{ item.label }}</dt>
            <dd>{{ item.value }}</dd>
          </template>
        </dl>
        <p
          v-if="props.detail.mutation.warning"
          class="rr-document-inspector__microcopy"
        >
          {{ props.detail.mutation.warning }}
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
          <span class="rr-document-inspector__action-label">{{ $t('documents.actions.groups.explore') }}</span>
          <div class="rr-document-inspector__action-row">
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
              @click="emit('downloadText', props.detail.id)"
            >
              {{ $t('documents.details.downloadText') }}
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
  gap: 1rem;
  padding: 1.1rem;
  border: 1px solid rgba(15, 23, 42, 0.07);
  border-radius: 1.25rem;
  background: rgba(255, 255, 255, 0.96);
  box-shadow: 0 18px 34px rgba(15, 23, 42, 0.05);
}

.rr-document-inspector__header,
.rr-document-inspector__section,
.rr-document-inspector__actions {
  display: grid;
  gap: 0.75rem;
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

.rr-document-inspector__status-row {
  display: flex;
  flex-wrap: wrap;
  gap: 0.4rem;
}

.rr-document-inspector__lead {
  padding: 0.85rem 0.95rem;
  border-radius: 0.9rem;
  background: rgba(247, 249, 252, 0.92);
  color: rgba(15, 23, 42, 0.72);
  font-size: 0.94rem;
  line-height: 1.55;
}

.rr-document-inspector__section {
  padding-top: 0.9rem;
  border-top: 1px solid rgba(15, 23, 42, 0.07);
}

.rr-document-inspector__section-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
}

.rr-document-inspector__preview {
  max-height: 18rem;
  overflow: auto;
  padding: 0.9rem 0.95rem;
  border-radius: 0.9rem;
  background: rgba(247, 249, 252, 0.92);
  color: rgba(15, 23, 42, 0.82);
  font-size: 0.93rem;
  line-height: 1.65;
  white-space: pre-wrap;
}

.rr-document-inspector__microcopy {
  color: rgba(15, 23, 42, 0.56);
  font-size: 0.9rem;
  line-height: 1.45;
}

.rr-document-inspector__facts {
  display: grid;
  grid-template-columns: minmax(0, 1fr) minmax(0, auto);
  gap: 0.55rem 1rem;
  margin: 0;
}

.rr-document-inspector__facts dt {
  font-size: 0.84rem;
  font-weight: 600;
  color: rgba(15, 23, 42, 0.56);
}

.rr-document-inspector__facts dd {
  margin: 0;
  text-align: right;
  color: rgba(15, 23, 42, 0.88);
  font-size: 0.93rem;
}

.rr-document-inspector__actions {
  gap: 1rem;
  padding-top: 0.9rem;
  border-top: 1px solid rgba(15, 23, 42, 0.07);
}

.rr-document-inspector__action-group {
  display: grid;
  gap: 0.6rem;
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

@media (min-width: 1025px) {
  .rr-document-inspector {
    position: sticky;
    top: 1rem;
    max-height: calc(100vh - 7rem);
    overflow: auto;
  }
}

@media (max-width: 1024px) {
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

  .rr-document-inspector__actions {
    gap: 0.85rem;
  }
}

@media (max-width: 640px) {
  .rr-document-inspector__facts {
    grid-template-columns: 1fr;
  }

  .rr-document-inspector__facts dd {
    text-align: left;
  }
}
</style>
