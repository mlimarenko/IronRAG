<script setup lang="ts">
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { useDisplayFormatters } from 'src/composables/useDisplayFormatters'
import type { TechnicalFactRow } from 'src/models/ui/documents'

const props = defineProps<{
  items: TechnicalFactRow[]
  totalCount?: number
}>()

const { t } = useI18n()
const { enumLabel, formatDateTime } = useDisplayFormatters()
const expanded = ref(false)
const visibleLimit = 10

interface FactGroupEntry {
  id: string
  kind: TechnicalFactRow['kind']
  canonicalValueLabel: string
  displayValue: string
  qualifiers: TechnicalFactRow['qualifiers']
  supportSegments: TechnicalFactRow['supportSegments']
  supportChunkCount: number
  confidence: number | null
  extractionKinds: string[]
  conflictGroupIds: string[]
  occurrenceCount: number
  lastSeenAt: string
}

interface FactKindSection {
  kind: TechnicalFactRow['kind']
  label: string
  entries: FactGroupEntry[]
}

const kindSections = computed<FactKindSection[]>(() => {
  const byKind = new Map<TechnicalFactRow['kind'], Map<string, FactGroupEntry>>()
  for (const item of props.items) {
    const kindMap = byKind.get(item.kind) ?? new Map<string, FactGroupEntry>()
    const groupKey = `${item.kind}::${item.canonicalValueLabel}`
    const existing = kindMap.get(groupKey)
    if (!existing) {
      kindMap.set(groupKey, {
        id: groupKey,
        kind: item.kind,
        canonicalValueLabel: item.canonicalValueLabel,
        displayValue: item.displayValue,
        qualifiers: item.qualifiers,
        supportSegments: item.supportSegments,
        supportChunkCount: item.supportChunkIds.length,
        confidence: item.confidence,
        extractionKinds: [item.extractionKind],
        conflictGroupIds: item.conflictGroupId ? [item.conflictGroupId] : [],
        occurrenceCount: 1,
        lastSeenAt: item.createdAt,
      })
      byKind.set(item.kind, kindMap)
      continue
    }

    const qualifierKeys = new Set(
      existing.qualifiers.map((qualifier) => `${qualifier.key}:${qualifier.value}`),
    )
    for (const qualifier of item.qualifiers) {
      const key = `${qualifier.key}:${qualifier.value}`
      if (!qualifierKeys.has(key)) {
        existing.qualifiers.push(qualifier)
        qualifierKeys.add(key)
      }
    }

    const supportSegmentIds = new Set(existing.supportSegments.map((segment) => segment.segmentId))
    for (const support of item.supportSegments) {
      if (!supportSegmentIds.has(support.segmentId)) {
        existing.supportSegments.push(support)
        supportSegmentIds.add(support.segmentId)
      }
    }

    existing.supportChunkCount = Math.max(existing.supportChunkCount, item.supportChunkIds.length)
    existing.confidence =
      existing.confidence === null
        ? item.confidence
        : item.confidence === null
          ? existing.confidence
          : Math.max(existing.confidence, item.confidence)
    if (!existing.extractionKinds.includes(item.extractionKind)) {
      existing.extractionKinds.push(item.extractionKind)
    }
    if (item.conflictGroupId && !existing.conflictGroupIds.includes(item.conflictGroupId)) {
      existing.conflictGroupIds.push(item.conflictGroupId)
    }
    existing.occurrenceCount += 1
    existing.lastSeenAt =
      Date.parse(item.createdAt) > Date.parse(existing.lastSeenAt)
        ? item.createdAt
        : existing.lastSeenAt
  }

  return Array.from(byKind.entries())
    .map(([kind, grouped]) => ({
      kind,
      label: enumLabel('documents.details.factKinds', kind, kind),
      entries: Array.from(grouped.values()).sort((left, right) =>
        left.canonicalValueLabel.localeCompare(right.canonicalValueLabel),
      ),
    }))
    .sort((left, right) => left.label.localeCompare(right.label))
})

const flattenedCount = computed(() =>
  kindSections.value.reduce((sum, section) => sum + section.entries.length, 0),
)
const hiddenCount = computed(() => Math.max(0, flattenedCount.value - visibleLimit))

const visibleSections = computed<FactKindSection[]>(() => {
  if (expanded.value || flattenedCount.value <= visibleLimit) {
    return kindSections.value
  }
  let remaining = visibleLimit
  return kindSections.value
    .map((section) => {
      if (remaining <= 0) {
        return null
      }
      const slice = section.entries.slice(0, remaining)
      remaining -= slice.length
      return {
        ...section,
        entries: slice,
      }
    })
    .filter((section): section is FactKindSection => section !== null && section.entries.length > 0)
})

function confidenceLabel(value: number | null): string | null {
  if (value === null) {
    return null
  }
  return t('documents.details.confidenceValue', { percent: Math.round(value * 100) })
}

function extractionKindLabel(value: string): string {
  return enumLabel('documents.details.extractionKinds', value, value)
}
</script>

<template>
  <div class="rr-doc-facts">
    <p v-if="props.items.length === 0" class="rr-doc-facts__empty">
      {{ $t('documents.details.noTechnicalFacts') }}
    </p>

    <template v-else>
      <section v-for="section in visibleSections" :key="section.kind" class="rr-doc-facts__section">
        <div class="rr-doc-facts__section-head">
          <strong>{{ section.label }}</strong>
          <span>{{ section.entries.length }}</span>
        </div>

        <article
          v-for="entry in section.entries"
          :key="entry.id"
          class="rr-doc-facts__entry"
          :class="{ 'is-conflict': entry.conflictGroupIds.length > 0 }"
        >
          <div class="rr-doc-facts__value-row">
            <strong class="rr-doc-facts__value">{{ entry.canonicalValueLabel }}</strong>
            <span v-if="entry.occurrenceCount > 1" class="rr-doc-facts__count">
              {{ $t('documents.details.occurrenceCount', { count: entry.occurrenceCount }) }}
            </span>
            <span v-if="entry.conflictGroupIds.length > 0" class="rr-doc-facts__conflict">
              {{ $t('documents.details.conflictMarker') }}
            </span>
          </div>

          <p v-if="entry.displayValue !== entry.canonicalValueLabel" class="rr-doc-facts__display">
            {{ entry.displayValue }}
          </p>

          <div v-if="entry.qualifiers.length > 0" class="rr-doc-facts__chips">
            <span
              v-for="qualifier in entry.qualifiers"
              :key="`${qualifier.key}:${qualifier.value}`"
              class="rr-doc-facts__chip"
            >
              {{ qualifier.key }}={{ qualifier.value }}
            </span>
          </div>

          <div class="rr-doc-facts__support">
            <span class="rr-doc-facts__support-label">{{
              $t('documents.details.supportSegments')
            }}</span>
            <div class="rr-doc-facts__chips">
              <span
                v-for="support in entry.supportSegments"
                :key="support.segmentId"
                class="rr-doc-facts__chip"
              >
                #{{ support.ordinal + 1 }} · {{ support.label }}
              </span>
              <span v-if="entry.supportSegments.length === 0" class="rr-doc-facts__chip">
                {{ $t('documents.details.supportChunksChip', { count: entry.supportChunkCount }) }}
              </span>
            </div>
          </div>

          <div class="rr-doc-facts__meta">
            <span v-if="confidenceLabel(entry.confidence)">{{
              confidenceLabel(entry.confidence)
            }}</span>
            <span>{{ entry.extractionKinds.map(extractionKindLabel).join(' · ') }}</span>
            <span>{{ formatDateTime(entry.lastSeenAt) }}</span>
          </div>
        </article>
      </section>

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
.rr-doc-facts {
  display: grid;
  gap: 0.78rem;
}

.rr-doc-facts__section {
  display: grid;
  gap: 0.58rem;
}

.rr-doc-facts__section-head,
.rr-doc-facts__value-row,
.rr-doc-facts__meta {
  display: flex;
  flex-wrap: wrap;
  gap: 0.45rem 0.6rem;
  align-items: center;
  justify-content: space-between;
}

.rr-doc-facts__section-head strong {
  color: rgba(15, 23, 42, 0.82);
  font-size: 0.84rem;
  letter-spacing: 0.04em;
  text-transform: uppercase;
}

.rr-doc-facts__section-head span,
.rr-doc-facts__meta {
  color: rgba(15, 23, 42, 0.56);
  font-size: 0.78rem;
}

.rr-doc-facts__entry {
  display: grid;
  gap: 0.48rem;
  padding: 0.78rem 0.82rem;
  border: 1px solid rgba(226, 232, 240, 0.88);
  border-radius: 0.95rem;
  background: rgba(255, 255, 255, 0.94);
}

.rr-doc-facts__entry.is-conflict {
  border-color: rgba(244, 114, 182, 0.28);
  background: rgba(255, 248, 252, 0.96);
}

.rr-doc-facts__value {
  color: rgba(15, 23, 42, 0.92);
  font-size: 0.95rem;
  line-height: 1.38;
  overflow-wrap: anywhere;
}

.rr-doc-facts__display,
.rr-doc-facts__empty {
  margin: 0;
}

.rr-doc-facts__display {
  color: rgba(15, 23, 42, 0.62);
  font-size: 0.84rem;
  line-height: 1.45;
}

.rr-doc-facts__chips {
  display: flex;
  flex-wrap: wrap;
  gap: 0.4rem 0.45rem;
}

.rr-doc-facts__chip,
.rr-doc-facts__count,
.rr-doc-facts__conflict {
  display: inline-flex;
  align-items: center;
  min-height: 1.5rem;
  padding: 0 0.55rem;
  border-radius: 999px;
  font-size: 0.76rem;
  font-weight: 700;
}

.rr-doc-facts__chip,
.rr-doc-facts__count {
  background: rgba(241, 245, 249, 0.94);
  color: rgba(15, 23, 42, 0.62);
}

.rr-doc-facts__conflict {
  background: rgba(251, 207, 232, 0.4);
  color: rgba(157, 23, 77, 0.9);
}

.rr-doc-facts__support {
  display: grid;
  gap: 0.32rem;
}

.rr-doc-facts__support-label {
  color: rgba(15, 23, 42, 0.48);
  font-size: 0.76rem;
  font-weight: 700;
  letter-spacing: 0.05em;
  text-transform: uppercase;
}

.rr-doc-facts__meta {
  justify-content: flex-start;
}

.rr-doc-facts__empty {
  color: rgba(15, 23, 42, 0.56);
  font-size: 0.9rem;
  line-height: 1.45;
}
</style>
