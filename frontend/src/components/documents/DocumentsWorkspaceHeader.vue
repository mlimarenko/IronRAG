<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import type { DocumentUploadFailure } from 'src/models/ui/documents'
import UploadDropzone from './UploadDropzone.vue'

const props = defineProps<{
  acceptedFormats: string[]
  maxSizeMb: number
  loading: boolean
  workspaceName: string | null
  libraryName: string | null
  uploadFailures: DocumentUploadFailure[]
}>()

const emit = defineEmits<{
  select: [files: File[]]
  clearFailures: []
}>()

const { t } = useI18n()

const uploadFailureSummary = computed(() => {
  const count = props.uploadFailures.length
  if (count === 0) {
    return null
  }
  return t('documents.uploadReport.summary', { count })
})

function uploadFailureMeta(failure: DocumentUploadFailure): string[] {
  const meta: string[] = []
  if (failure.detectedFormat) {
    meta.push(`${t('documents.uploadReport.labels.format')}: ${failure.detectedFormat}`)
  }
  if (failure.mimeType) {
    meta.push(`${t('documents.uploadReport.labels.mimeType')}: ${failure.mimeType}`)
  }
  if (failure.uploadLimitMb !== null) {
    meta.push(`${t('documents.uploadReport.labels.limit')}: ${String(failure.uploadLimitMb)} MB`)
  }
  return meta
}

function uploadFailureKindLabel(failure: DocumentUploadFailure): string | null {
  if (!failure.rejectionKind) {
    return null
  }
  const key = `documents.uploadReport.rejectionKinds.${failure.rejectionKind}`
  return t(key) === key ? failure.rejectionKind : t(key)
}
</script>

<template>
  <section class="rr-page-card rr-documents-workspace-header">
    <div class="rr-documents-workspace-header__copy">
      <span class="rr-documents-workspace-header__eyebrow">
        {{ $t('shell.documents') }}
      </span>
      <h1>{{ $t('documents.workspace.title') }}</h1>
      <p>{{ $t('documents.workspace.subtitle') }}</p>
      <div class="rr-documents-workspace-header__meta">
        <span>{{ `${$t('shell.workspace')} ${workspaceName || '—'}` }}</span>
        <span>{{ `${$t('shell.library')} ${libraryName || '—'}` }}</span>
      </div>
    </div>

    <div class="rr-documents-workspace-header__dropzone">
      <UploadDropzone
        :accepted-formats="acceptedFormats"
        :max-size-mb="maxSizeMb"
        :loading="loading"
        @select="emit('select', $event)"
      />
    </div>

    <section
      v-if="uploadFailures.length"
      class="rr-documents-workspace-header__report"
      role="status"
      aria-live="polite"
    >
      <div class="rr-documents-workspace-header__report-head">
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

      <ul class="rr-documents-workspace-header__report-list">
        <li
          v-for="failure in uploadFailures"
          :key="`${failure.fileName}:${failure.message}`"
          class="rr-documents-workspace-header__report-item"
        >
          <div class="rr-documents-workspace-header__report-line">
            <strong>{{ failure.fileName }}</strong>
            <span
              v-if="uploadFailureKindLabel(failure)"
              class="rr-documents-workspace-header__report-kind"
            >
              {{ uploadFailureKindLabel(failure) }}
            </span>
            <span>{{ failure.message }}</span>
          </div>
          <p v-if="failure.rejectionCause">
            <span>{{ $t('documents.uploadReport.labels.reason') }}:</span>
            {{ failure.rejectionCause }}
          </p>
          <p v-if="failure.operatorAction">
            <span>{{ $t('documents.uploadReport.labels.action') }}:</span>
            {{ failure.operatorAction }}
          </p>
          <p v-if="uploadFailureMeta(failure).length">
            {{ uploadFailureMeta(failure).join(' · ') }}
          </p>
        </li>
      </ul>
    </section>
  </section>
</template>
