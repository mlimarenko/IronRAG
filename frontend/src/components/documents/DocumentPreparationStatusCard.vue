<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import StatusPill from 'src/components/base/StatusPill.vue'
import { useDisplayFormatters } from 'src/composables/useDisplayFormatters'
import type {
  DocumentGraphCoverageKind,
  DocumentPreparationReadinessKind,
} from 'src/models/ui/documents'

const props = defineProps<{
  readinessKind: DocumentPreparationReadinessKind
  graphCoverageKind: DocumentGraphCoverageKind
  typedFactCoverage: number | null
  lastProcessingStage: string | null
  preparationState: string
  preparedSegmentCount: number
  technicalFactCount: number
  sourceFormat?: string | null
  normalizationProfile?: string | null
  updatedAt?: string | null
}>()

const { t } = useI18n()
const { enumLabel, formatDateTime } = useDisplayFormatters()

const readinessTone = computed(() => {
  switch (props.readinessKind) {
    case 'readable':
      return 'readable'
    case 'graph_sparse':
      return 'graph_sparse'
    case 'graph_ready':
      return 'graph_ready'
    case 'failed':
      return 'failed'
    case 'processing':
    default:
      return 'processing'
  }
})

const readinessLabel = computed(() =>
  enumLabel('documents.details.readinessKinds', props.readinessKind, props.readinessKind),
)

const graphCoverageLabel = computed(() =>
  enumLabel(
    'documents.details.graphCoverageKinds',
    props.graphCoverageKind,
    props.graphCoverageKind,
  ),
)

const preparationStateLabel = computed(() =>
  enumLabel('documents.details.preparationStates', props.preparationState, props.preparationState),
)

const stageLabel = computed(() =>
  props.lastProcessingStage
    ? enumLabel('documents.stage', props.lastProcessingStage, props.lastProcessingStage)
    : t('documents.details.noStage'),
)

const sourceFormatLabel = computed(() =>
  props.sourceFormat
    ? enumLabel('documents.details.sourceFormats', props.sourceFormat, props.sourceFormat)
    : '—',
)

const typedFactCoveragePercent = computed(() => {
  if (props.typedFactCoverage === null) {
    return null
  }
  return Math.max(0, Math.min(100, Math.round(props.typedFactCoverage * 100)))
})
</script>

<template>
  <article class="rr-doc-preparation-card">
    <div class="rr-doc-preparation-card__header">
      <div class="rr-doc-preparation-card__headline">
        <StatusPill :tone="readinessTone" :label="readinessLabel" />
        <span class="rr-doc-preparation-card__coverage">{{ graphCoverageLabel }}</span>
      </div>
      <p class="rr-doc-preparation-card__stamp">
        {{ $t('documents.details.updatedAt') }} · {{ formatDateTime(props.updatedAt ?? null) }}
      </p>
    </div>

    <div class="rr-doc-preparation-card__grid">
      <article class="rr-doc-preparation-card__metric">
        <span>{{ $t('documents.details.preparedSegmentsCount') }}</span>
        <strong>{{ props.preparedSegmentCount }}</strong>
      </article>
      <article class="rr-doc-preparation-card__metric">
        <span>{{ $t('documents.details.technicalFactsCount') }}</span>
        <strong>{{ props.technicalFactCount }}</strong>
      </article>
      <article class="rr-doc-preparation-card__metric">
        <span>{{ $t('documents.details.preparationState') }}</span>
        <strong>{{ preparationStateLabel }}</strong>
      </article>
      <article class="rr-doc-preparation-card__metric">
        <span>{{ $t('documents.details.lastProcessingStage') }}</span>
        <strong>{{ stageLabel }}</strong>
      </article>
    </div>

    <div class="rr-doc-preparation-card__coverage-bar">
      <div class="rr-doc-preparation-card__coverage-meta">
        <span>{{ $t('documents.details.typedFactCoverage') }}</span>
        <strong>
          {{
            typedFactCoveragePercent === null
              ? $t('documents.details.notAvailable')
              : $t('documents.details.typedFactCoverageValue', {
                  percent: typedFactCoveragePercent,
                })
          }}
        </strong>
      </div>
      <div class="rr-doc-preparation-card__progress-track">
        <div
          class="rr-doc-preparation-card__progress-fill"
          :style="{ width: `${String(typedFactCoveragePercent ?? 0)}%` }"
        />
      </div>
    </div>

    <div class="rr-doc-preparation-card__foot">
      <span>{{ $t('documents.details.sourceFormat') }}: {{ sourceFormatLabel }}</span>
      <span
        >{{ $t('documents.details.normalizationProfile') }}:
        {{ props.normalizationProfile ?? '—' }}</span
      >
    </div>
  </article>
</template>

<style scoped lang="scss">
.rr-doc-preparation-card {
  display: grid;
  gap: 0.8rem;
  padding: 0.85rem 0.9rem;
  border: 1px solid rgba(37, 99, 235, 0.12);
  border-radius: 1rem;
  background: linear-gradient(180deg, rgba(248, 250, 255, 0.98), rgba(255, 255, 255, 0.96));
}

.rr-doc-preparation-card__header,
.rr-doc-preparation-card__headline,
.rr-doc-preparation-card__coverage-meta,
.rr-doc-preparation-card__foot {
  display: flex;
  flex-wrap: wrap;
  gap: 0.5rem 0.75rem;
  align-items: center;
}

.rr-doc-preparation-card__header,
.rr-doc-preparation-card__coverage-meta {
  justify-content: space-between;
}

.rr-doc-preparation-card__coverage {
  padding: 0.24rem 0.55rem;
  border-radius: 999px;
  background: rgba(226, 232, 240, 0.74);
  color: rgba(15, 23, 42, 0.7);
  font-size: 0.76rem;
  font-weight: 700;
  letter-spacing: 0.04em;
  text-transform: uppercase;
}

.rr-doc-preparation-card__stamp,
.rr-doc-preparation-card__foot {
  margin: 0;
  color: rgba(15, 23, 42, 0.56);
  font-size: 0.8rem;
  line-height: 1.4;
}

.rr-doc-preparation-card__grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 0.55rem;
}

.rr-doc-preparation-card__metric {
  display: grid;
  gap: 0.24rem;
  padding: 0.68rem 0.72rem;
  border: 1px solid rgba(226, 232, 240, 0.92);
  border-radius: 0.9rem;
  background: rgba(255, 255, 255, 0.92);
}

.rr-doc-preparation-card__metric span {
  color: rgba(15, 23, 42, 0.48);
  font-size: 0.72rem;
  font-weight: 700;
  letter-spacing: 0.06em;
  text-transform: uppercase;
}

.rr-doc-preparation-card__metric strong {
  color: rgba(15, 23, 42, 0.9);
  font-size: 0.92rem;
  line-height: 1.35;
}

.rr-doc-preparation-card__coverage-bar {
  display: grid;
  gap: 0.45rem;
}

.rr-doc-preparation-card__coverage-meta span {
  color: rgba(15, 23, 42, 0.56);
  font-size: 0.8rem;
  font-weight: 700;
  letter-spacing: 0.04em;
  text-transform: uppercase;
}

.rr-doc-preparation-card__coverage-meta strong {
  color: rgba(15, 23, 42, 0.88);
  font-size: 0.86rem;
}

.rr-doc-preparation-card__progress-track {
  height: 0.55rem;
  border-radius: 999px;
  background: rgba(226, 232, 240, 0.78);
  overflow: hidden;
}

.rr-doc-preparation-card__progress-fill {
  height: 100%;
  border-radius: inherit;
  background: linear-gradient(90deg, rgba(59, 130, 246, 0.82), rgba(15, 118, 110, 0.82));
}

@media (max-width: 620px) {
  .rr-doc-preparation-card__grid {
    grid-template-columns: 1fr;
  }
}
</style>
