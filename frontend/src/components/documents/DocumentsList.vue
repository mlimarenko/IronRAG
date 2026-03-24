<script setup lang="ts">
import StatusPill from 'src/components/base/StatusPill.vue'
import type { DocumentRowSummary } from 'src/models/ui/documents'

const props = defineProps<{
  rows: DocumentRowSummary[]
  selectedId?: string | null
}>()

const emit = defineEmits<{
  detail: [id: string]
  append: [id: string]
  replace: [id: string]
  retry: [id: string]
  remove: [id: string]
}>()

function hasDetailTarget(row: DocumentRowSummary): boolean {
  return row.detailAvailable && row.id.trim().length > 0
}
</script>

<template>
  <section class="rr-documents-list" role="list">
    <article
      v-for="row in props.rows"
      :key="row.id"
      class="rr-document-row"
      :class="{ 'is-selected': row.id === props.selectedId }"
      role="listitem"
    >
      <component
        :is="hasDetailTarget(row) ? 'button' : 'div'"
        class="rr-document-row__surface"
        :class="{ 'is-static': !hasDetailTarget(row) }"
        v-bind="hasDetailTarget(row) ? { type: 'button' } : {}"
        :aria-pressed="hasDetailTarget(row) ? String(row.id === props.selectedId) : undefined"
        @click="hasDetailTarget(row) ? emit('detail', row.id) : undefined"
      >
        <div class="rr-document-row__main">
          <div class="rr-document-row__copy">
            <strong class="rr-document-row__title">{{ row.fileName }}</strong>
            <span class="rr-document-row__meta">
              {{ [row.fileType, row.fileSizeLabel].join(' · ') }}
            </span>
          </div>
        </div>

        <div class="rr-document-row__status">
          <StatusPill
            :tone="row.status"
            :label="row.statusLabel"
          />
          <span
            v-if="row.mutationLabel"
            class="rr-document-row__hint"
          >
            {{ row.mutationLabel }}
          </span>
          <span class="rr-document-row__updated">
            {{ row.activityLabel }}
          </span>
        </div>

        <div class="rr-document-row__actions">
          <button
            v-if="row.canAppend"
            class="rr-button rr-button--ghost rr-button--tiny"
            type="button"
            @click.stop="emit('append', row.id)"
          >
            {{ $t('documents.actions.append') }}
          </button>
          <button
            v-if="row.canReplace"
            class="rr-button rr-button--ghost rr-button--tiny"
            type="button"
            @click.stop="emit('replace', row.id)"
          >
            {{ $t('documents.actions.replace') }}
          </button>
          <button
            v-if="row.canRetry"
            class="rr-button rr-button--ghost rr-button--tiny"
            type="button"
            @click.stop="emit('retry', row.id)"
          >
            {{ $t('documents.actions.retry') }}
          </button>
          <button
            v-if="row.canRemove"
            class="rr-button rr-button--ghost rr-button--tiny is-danger"
            type="button"
            @click.stop="emit('remove', row.id)"
          >
            {{ $t('documents.actions.remove') }}
          </button>
        </div>
      </component>
    </article>
  </section>
</template>

<style scoped>
.rr-documents-list {
  display: grid;
  gap: 0.35rem;
  padding: 0.2rem 0.3rem 0.35rem;
}

.rr-document-row {
  border-radius: 0.9rem;
}

.rr-document-row__surface {
  width: 100%;
  display: grid;
  grid-template-columns: minmax(0, 1fr) auto auto;
  gap: 0.75rem;
  align-items: center;
  padding: 0.65rem 0.75rem;
  border: 1px solid rgba(148, 163, 184, 0.2);
  border-radius: 0.9rem;
  background: rgba(248, 250, 252, 0.78);
  transition:
    border-color 140ms ease,
    background-color 140ms ease,
    box-shadow 140ms ease;
  text-align: left;
}

.rr-document-row__surface:not(.is-static) {
  cursor: pointer;
}

.rr-document-row__surface:not(.is-static):hover {
  border-color: rgba(99, 102, 241, 0.14);
  background: rgba(243, 246, 255, 0.92);
  box-shadow: none;
}

.rr-document-row.is-selected .rr-document-row__surface {
  border-color: rgba(99, 102, 241, 0.18);
  background: rgba(241, 245, 255, 0.96);
  box-shadow: inset 3px 0 0 rgba(99, 102, 241, 0.72);
}

.rr-document-row__main,
.rr-document-row__copy {
  min-width: 0;
}

.rr-document-row__copy {
  display: grid;
  gap: 0.25rem;
}

.rr-document-row__title {
  display: block;
  color: rgba(15, 23, 42, 0.94);
  font-size: 0.98rem;
  font-weight: 700;
  line-height: 1.2;
}

.rr-document-row__meta,
.rr-document-row__hint,
.rr-document-row__updated,
.rr-document-row__stage {
  color: rgba(15, 23, 42, 0.56);
  font-size: 0.82rem;
  line-height: 1.35;
}

.rr-document-row__status {
  display: grid;
  justify-items: end;
  gap: 0.25rem;
  min-width: 6rem;
}

.rr-document-row__actions {
  display: flex;
  flex-wrap: wrap;
  justify-content: flex-end;
  gap: 0.3rem;
}

@media (max-width: 720px) {
  .rr-documents-list {
    padding-inline: 0;
  }

  .rr-document-row__surface {
    grid-template-columns: minmax(0, 1fr);
    justify-items: start;
  }

  .rr-document-row__status {
    justify-items: start;
    min-width: 0;
  }

  .rr-document-row__actions {
    justify-content: flex-start;
  }
}
</style>
