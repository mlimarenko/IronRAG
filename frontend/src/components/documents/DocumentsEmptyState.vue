<script setup lang="ts">
import FeedbackState from 'src/components/design-system/FeedbackState.vue'
import UploadDropzone from './UploadDropzone.vue'

const props = defineProps<{
  loading: boolean
  acceptedFormats: string[]
  maxSizeMb: number
  uploadLoading: boolean
  hasDocuments?: boolean
  hasActiveFilters?: boolean
}>()

const emit = defineEmits<{
  clearFilters: []
  select: [files: File[]]
  openAddLink: []
}>()
</script>

<template>
  <section
    v-if="props.loading"
    class="rr-docs-empty-state rr-docs-empty-state--loading"
    aria-live="polite"
  >
    <div class="rr-docs-empty-state__intro">
      <span class="rr-docs-empty-state__eyebrow">{{ $t('documents.workspace.title') }}</span>
      <h2>{{ $t('documents.loading') }}</h2>
      <p>{{ $t('documents.workspace.loadingDescription') }}</p>
    </div>

    <div class="rr-docs-empty-state__skeleton" aria-hidden="true">
      <span class="rr-docs-empty-state__skeleton-pill" />
      <span class="rr-docs-empty-state__skeleton-line is-wide" />
      <span class="rr-docs-empty-state__skeleton-line is-medium" />
      <span class="rr-docs-empty-state__skeleton-line is-short" />
    </div>
  </section>

  <section
    v-else-if="!props.hasDocuments && !props.hasActiveFilters"
    class="rr-docs-empty-state rr-docs-empty-state--empty"
  >
    <div class="rr-docs-empty-state__intro">
      <span class="rr-docs-empty-state__eyebrow">{{ $t('documents.workspace.title') }}</span>
      <h2>{{ $t('documents.workspace.emptyTitle') }}</h2>
      <p>{{ $t('documents.workspace.emptyDescription') }}</p>
      <p class="rr-docs-empty-state__note">
        {{ $t('documents.maxSize', { size: props.maxSizeMb }) }} ·
        {{ $t('documents.uploadQueuedHint') }}
      </p>

      <div class="rr-docs-empty-state__action-row">
        <UploadDropzone
          :accepted-formats="props.acceptedFormats"
          :max-size-mb="props.maxSizeMb"
          :loading="props.uploadLoading"
          variant="inline"
          :show-meta="false"
          @select="emit('select', $event)"
        />
        <button
          type="button"
          class="rr-button rr-button--secondary rr-button--compact"
          @click="emit('openAddLink')"
        >
          {{ $t('documents.actions.addLink') }}
        </button>
      </div>
    </div>
  </section>

  <FeedbackState
    v-else
    kind="sparse"
    :title="$t('documents.workspace.noMatchTitle')"
    :message="$t('documents.workspace.noMatchDescription')"
    :action-label="$t('documents.actions.clearFilters')"
    @action="emit('clearFilters')"
  />
</template>

<style scoped lang="scss">
.rr-docs-empty-state {
  display: grid;
  gap: 1rem;
  padding: 1rem;
  border: 1px solid rgba(226, 232, 240, 0.84);
  border-radius: 16px;
  background: rgba(255, 255, 255, 0.98);
}

.rr-docs-empty-state--empty {
  width: min(100%, 44rem);
  min-height: 0;
  justify-items: start;
  margin: 0 auto;
}

.rr-docs-empty-state__intro {
  display: grid;
  gap: 0.6rem;
  align-content: start;
  width: min(100%, 34rem);
}

.rr-docs-empty-state__eyebrow {
  color: rgba(100, 116, 139, 0.82);
  font-size: 0.72rem;
  font-weight: 700;
  letter-spacing: 0.08em;
  text-transform: uppercase;
}

.rr-docs-empty-state__intro h2 {
  margin: 0;
  color: rgba(15, 23, 42, 0.96);
  font-size: 1.2rem;
  line-height: 1.15;
  letter-spacing: -0.03em;
}

.rr-docs-empty-state__intro p {
  max-width: 34rem;
  margin: 0;
  color: rgba(71, 85, 105, 0.88);
  font-size: 0.9rem;
  line-height: 1.55;
}

.rr-docs-empty-state__note {
  color: rgba(100, 116, 139, 0.92);
  font-size: 0.8rem;
}

.rr-docs-empty-state__action-row {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 0.65rem;
  margin-top: 0.15rem;
}

.rr-docs-empty-state__action-row :deep(.rr-upload-dropzone) {
  min-width: 12.5rem;
}

.rr-docs-empty-state__skeleton {
  display: grid;
  gap: 0.7rem;
  width: min(100%, 42rem);
}

.rr-docs-empty-state__skeleton-pill,
.rr-docs-empty-state__skeleton-line {
  display: block;
  border-radius: 999px;
  background: linear-gradient(
    90deg,
    rgba(226, 232, 240, 0.78) 20%,
    rgba(241, 245, 249, 0.96) 38%,
    rgba(226, 232, 240, 0.78) 56%
  );
  background-size: 220% 100%;
  animation: rr-docs-empty-state-shimmer 1.6s ease-in-out infinite;
}

.rr-docs-empty-state__skeleton-pill {
  width: 9rem;
  height: 1.45rem;
}

.rr-docs-empty-state__skeleton-line {
  height: 0.92rem;
}

.rr-docs-empty-state__skeleton-line.is-wide {
  width: min(100%, 36rem);
}

.rr-docs-empty-state__skeleton-line.is-medium {
  width: min(100%, 28rem);
}

.rr-docs-empty-state__skeleton-line.is-short {
  width: min(100%, 20rem);
}

@keyframes rr-docs-empty-state-shimmer {
  0% {
    background-position: 100% 50%;
  }

  100% {
    background-position: 0% 50%;
  }
}

@media (max-width: 640px) {
  .rr-docs-empty-state {
    padding: 0.95rem 0.9rem;
  }

  .rr-docs-empty-state__action-row {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
  }

  .rr-docs-empty-state__action-row :deep(.rr-button),
  .rr-docs-empty-state__action-row :deep(.rr-upload-dropzone) {
    width: 100%;
  }
}
</style>
