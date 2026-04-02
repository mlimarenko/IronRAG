<script setup lang="ts">
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { useDisplayFormatters } from 'src/composables/useDisplayFormatters'
import type { PreparedSegmentRow } from 'src/models/ui/documents'

const props = defineProps<{
  items: PreparedSegmentRow[]
  totalCount?: number
}>()

const { t } = useI18n()
const { enumLabel } = useDisplayFormatters()
const expanded = ref(false)
const visibleLimit = 8

const hiddenCount = computed(() => Math.max(0, props.items.length - visibleLimit))
const visibleItems = computed(() =>
  expanded.value || props.items.length <= visibleLimit
    ? props.items
    : props.items.slice(0, visibleLimit),
)

function blockKindLabel(kind: PreparedSegmentRow['kind']): string {
  return enumLabel('documents.details.segmentKinds', kind, kind)
}

function headingTrailLabel(item: PreparedSegmentRow): string | null {
  if (item.headingTrail.length > 0) {
    return item.headingTrail.join(' / ')
  }
  if (item.sectionPath.length > 0) {
    return item.sectionPath.join(' / ')
  }
  return null
}

function locationChips(item: PreparedSegmentRow): string[] {
  const chips: string[] = []
  if (item.location.pageNumber !== null) {
    chips.push(t('documents.details.pageChip', { page: item.location.pageNumber }))
  }
  if (item.location.startOffset !== null && item.location.endOffset !== null) {
    chips.push(
      t('documents.details.offsetsChip', {
        start: item.location.startOffset,
        end: item.location.endOffset,
      }),
    )
  }
  if (item.location.supportChunkCount > 0) {
    chips.push(t('documents.details.supportChunksChip', { count: item.location.supportChunkCount }))
  }
  if (item.codeLanguage) {
    chips.push(t('documents.details.codeLanguageChip', { language: item.codeLanguage }))
  }
  if (item.tableCoordinates) {
    chips.push(
      t('documents.details.tableCellChip', {
        row: item.tableCoordinates.rowIndex + 1,
        column: item.tableCoordinates.columnIndex + 1,
      }),
    )
  }
  return chips
}
</script>

<template>
  <div class="rr-doc-segments">
    <p v-if="props.items.length === 0" class="rr-doc-segments__empty">
      {{ $t('documents.details.noPreparedSegments') }}
    </p>

    <template v-else>
      <article v-for="item in visibleItems" :key="item.id" class="rr-doc-segments__item">
        <div class="rr-doc-segments__topline">
          <span class="rr-doc-segments__kind">{{ blockKindLabel(item.kind) }}</span>
          <span class="rr-doc-segments__ordinal">#{{ item.ordinal + 1 }}</span>
        </div>

        <p v-if="headingTrailLabel(item)" class="rr-doc-segments__trail">
          {{ headingTrailLabel(item) }}
        </p>

        <p class="rr-doc-segments__excerpt">
          {{ item.excerpt }}
        </p>

        <div class="rr-doc-segments__chips">
          <span v-for="chip in locationChips(item)" :key="chip" class="rr-doc-segments__chip">
            {{ chip }}
          </span>
        </div>
      </article>

      <button
        v-if="hiddenCount > 0 || expanded"
        type="button"
        class="rr-button rr-button--ghost rr-button--tiny"
        @click="expanded = !expanded"
      >
        {{
          expanded
            ? $t('documents.details.showLess')
            : $t('documents.details.showMoreCount', {
                count: hiddenCount,
                total: props.totalCount ?? props.items.length,
              })
        }}
      </button>
    </template>
  </div>
</template>

<style scoped lang="scss">
.rr-doc-segments {
  display: grid;
  gap: 0.68rem;
}

.rr-doc-segments__item {
  display: grid;
  gap: 0.45rem;
  padding: 0.78rem 0.82rem;
  border: 1px solid rgba(226, 232, 240, 0.86);
  border-radius: 0.95rem;
  background: rgba(255, 255, 255, 0.94);
}

.rr-doc-segments__topline,
.rr-doc-segments__chips {
  display: flex;
  flex-wrap: wrap;
  gap: 0.4rem 0.45rem;
  align-items: center;
}

.rr-doc-segments__kind,
.rr-doc-segments__chip,
.rr-doc-segments__ordinal {
  display: inline-flex;
  align-items: center;
  min-height: 1.5rem;
  padding: 0 0.55rem;
  border-radius: 999px;
  font-size: 0.76rem;
  font-weight: 700;
}

.rr-doc-segments__kind {
  background: rgba(59, 130, 246, 0.12);
  color: rgba(37, 99, 235, 0.92);
}

.rr-doc-segments__ordinal,
.rr-doc-segments__chip {
  background: rgba(241, 245, 249, 0.94);
  color: rgba(15, 23, 42, 0.62);
}

.rr-doc-segments__trail,
.rr-doc-segments__excerpt,
.rr-doc-segments__empty {
  margin: 0;
}

.rr-doc-segments__trail {
  color: rgba(15, 23, 42, 0.6);
  font-size: 0.82rem;
  line-height: 1.45;
}

.rr-doc-segments__excerpt {
  color: rgba(15, 23, 42, 0.88);
  font-size: 0.9rem;
  line-height: 1.58;
  white-space: pre-wrap;
}

.rr-doc-segments__empty {
  color: rgba(15, 23, 42, 0.56);
  font-size: 0.9rem;
  line-height: 1.45;
}
</style>
