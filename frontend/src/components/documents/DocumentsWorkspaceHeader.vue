<script setup lang="ts">
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import PageHeader from 'src/components/design-system/PageHeader.vue'
import type { DocumentUploadFailure } from 'src/models/ui/documents'
import UploadDropzone from './UploadDropzone.vue'

const props = defineProps<{
  acceptedFormats: string[]
  maxSizeMb: number
  loading: boolean
  totalCount?: number
  activeCount?: number
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
  if (count === 0) {
    return null
  }
  return t('documents.uploadReport.summary', { count })
})

const contextNote = computed(() => {
  const totalCount = props.totalCount ?? 0
  const activeCount = props.activeCount ?? 0
  if (totalCount > 0 && activeCount > 0) {
    return t('documents.workspace.contextActive', { total: totalCount, active: activeCount })
  }
  if (totalCount > 0) {
    return t('documents.workspace.contextTotal', { count: totalCount })
  }
  return t('documents.workspace.contextEmpty')
})

function uploadFailureKindLabel(failure: DocumentUploadFailure): string | null {
  if (!failure.rejectionKind) {
    return null
  }
  const key = `documents.uploadReport.rejectionKinds.${failure.rejectionKind}`
  return t(key) === key ? failure.rejectionKind : t(key)
}

function openUploader(): void {
  uploadRef.value?.openPicker()
}

defineExpose({
  openUploader,
})
</script>

<template>
  <header class="rr-documents-header">
    <PageHeader
      compact
      :eyebrow="$t('shell.documents')"
      :title="$t('documents.workspace.title')"
      :subtitle="$t('documents.workspace.subtitle')"
    >
      <template #meta>
        <p
          v-if="contextNote"
          class="rr-documents-header__summary"
        >
          {{ contextNote }}
        </p>
      </template>

      <template #actions>
        <UploadDropzone
          ref="uploadRef"
          :accepted-formats="acceptedFormats"
          :max-size-mb="maxSizeMb"
          :loading="loading"
          :has-documents="Boolean(hasDocuments)"
          @select="emit('select', $event)"
        />
      </template>
    </PageHeader>

    <section
      v-if="uploadFailures.length"
      class="rr-documents-header__alert"
      role="status"
      aria-live="polite"
    >
      <div class="rr-documents-header__alert-summary">
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

      <details class="rr-documents-header__alert-details">
        <summary>{{ $t('documents.uploadReport.showDetails') }}</summary>
        <ul class="rr-documents-header__alert-list">
          <li
            v-for="failure in uploadFailures"
            :key="`${failure.fileName}:${failure.message}`"
            class="rr-documents-header__alert-item"
          >
            <div class="rr-documents-header__alert-line">
              <strong>{{ failure.fileName }}</strong>
              <span
                v-if="uploadFailureKindLabel(failure)"
                class="rr-documents-header__alert-kind"
              >
                {{ uploadFailureKindLabel(failure) }}
              </span>
              <span>{{ failure.message }}</span>
            </div>
          </li>
        </ul>
      </details>
    </section>
  </header>
</template>

<style scoped lang="scss">
.rr-documents-header {
  display: grid;
  gap: 0.85rem;
  padding-inline: 0.1rem;
}

.rr-documents-header :deep(.rr-page-header) {
  display: grid;
  grid-template-columns: minmax(0, 1fr) auto;
  gap: 1rem;
  align-items: start;
}

.rr-documents-header :deep(.rr-page-header__copy) {
  gap: 0.45rem;
}

.rr-documents-header :deep(.rr-page-header__title) {
  max-width: none;
  font-size: clamp(1.5rem, 2.3vw, 2.2rem);
  line-height: 1;
  letter-spacing: -0.04em;
}

.rr-documents-header :deep(.rr-page-header__description) {
  max-width: 62ch;
  color: var(--rr-text-muted);
  font-size: 0.95rem;
  line-height: 1.55;
}

.rr-documents-header :deep(.rr-page-header__actions) {
  justify-self: end;
}

.rr-documents-header__summary {
  margin: 0;
  color: var(--rr-text-muted);
  font-size: 0.9rem;
  line-height: 1.5;
}

.rr-documents-header__alert {
  display: grid;
  gap: 0.75rem;
  padding: 1rem 1.1rem;
  border-radius: 1rem;
  border: 1px solid rgba(239, 68, 68, 0.15);
  background: rgba(254, 242, 242, 0.9);
}

.rr-documents-header__alert-summary {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 1rem;
}

.rr-documents-header__alert-summary strong {
  display: block;
  margin-bottom: 0.2rem;
}

.rr-documents-header__alert-summary p {
  margin: 0;
  color: rgba(127, 29, 29, 0.74);
}

.rr-documents-header__alert-details {
  color: rgba(127, 29, 29, 0.86);
}

.rr-documents-header__alert-details summary {
  cursor: pointer;
  font-weight: 600;
}

.rr-documents-header__alert-list {
  display: grid;
  gap: 0.55rem;
  padding-left: 1rem;
  margin: 0.75rem 0 0;
}

.rr-documents-header__alert-item {
  color: rgba(127, 29, 29, 0.84);
}

.rr-documents-header__alert-line {
  display: inline-flex;
  flex-wrap: wrap;
  gap: 0.45rem;
}

.rr-documents-header__alert-kind {
  padding: 0.15rem 0.45rem;
  border-radius: 999px;
  background: rgba(127, 29, 29, 0.08);
  font-size: 0.75rem;
  font-weight: 700;
}

@media (max-width: 1100px) {
  .rr-documents-header :deep(.rr-page-header) {
    grid-template-columns: minmax(0, 1fr);
    align-items: start;
  }

  .rr-documents-header :deep(.rr-page-header__actions) {
    justify-self: start;
  }
}

@media (max-width: 720px) {
  .rr-documents-header {
    gap: 0.875rem;
  }

  .rr-documents-header :deep(.rr-page-header__title) {
    font-size: clamp(1.85rem, 10vw, 2.6rem);
  }

  .rr-documents-header__alert-summary {
    flex-direction: column;
  }
}
</style>
