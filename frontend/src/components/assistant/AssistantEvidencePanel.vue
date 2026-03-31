<script setup lang="ts">
import { computed } from 'vue'
import type {
  KnowledgeBundleChunkReference,
  KnowledgeBundleEntityReference,
  KnowledgeBundleEvidenceReference,
  KnowledgeBundleRelationReference,
  KnowledgeContextBundleDetail,
  QueryExecutionDetail,
} from 'src/services/api/query'
import { useDisplayFormatters } from 'src/composables/useDisplayFormatters'
import { useI18n } from 'vue-i18n'

interface ReferenceListItem {
  id: string
  rank: number
  score: number
  label: string
}

const props = withDefaults(defineProps<{
  libraryName: string
  executing: boolean
  error: string | null
  execution: QueryExecutionDetail | null
  bundle: KnowledgeContextBundleDetail | null
  chunkReferences: KnowledgeBundleChunkReference[]
  entityReferences: KnowledgeBundleEntityReference[]
  relationReferences: KnowledgeBundleRelationReference[]
  evidenceReferences: KnowledgeBundleEvidenceReference[]
  closable?: boolean
}>(), {
  closable: false,
})

const emit = defineEmits<{
  (event: 'open-graph'): void
  (event: 'open-documents'): void
  (event: 'close'): void
}>()

const { t } = useI18n()
const { formatDateTime, shortIdentifier } = useDisplayFormatters()

function localizedExecutionState(state: string): string {
  const key = `assistant.evidence.executionStates.${state}`
  const translated = t(key)
  return translated === key ? state : translated
}

function localizedBundleState(state: string): string {
  const key = `assistant.evidence.bundleStates.${state}`
  const translated = t(key)
  return translated === key ? state : translated
}

function referenceIdentifier(id: string): string {
  if (id.length <= 14) {
    return id
  }
  return `${id.slice(0, 8)}…${id.slice(-4)}`
}

function referenceLabel(kind: 'chunk' | 'entity' | 'relation' | 'evidence', id: string): string {
  return t(`assistant.evidence.labels.${kind}`, { id: referenceIdentifier(id) })
}

function topReferences<T extends { rank: number; score: number }>(
  kind: 'chunk' | 'entity' | 'relation' | 'evidence',
  items: T[],
  resolveId: (item: T) => string,
): ReferenceListItem[] {
  const dedupedItems = new Map<string, T>()
  for (const item of items) {
    const id = resolveId(item)
    const existing = dedupedItems.get(id)
    if (
      !existing ||
      item.rank < existing.rank ||
      (item.rank === existing.rank && item.score > existing.score)
    ) {
      dedupedItems.set(id, item)
    }
  }

  return Array.from(dedupedItems.values())
    .sort((left, right) => left.rank - right.rank || right.score - left.score)
    .slice(0, 4)
    .map((item) => {
      const id = resolveId(item)
      return {
        id,
        rank: item.rank,
        score: item.score,
        label: referenceLabel(kind, id),
      }
    })
}

const executionSummary = computed(() => {
  if (!props.execution) {
    return null
  }

  return {
    state: props.execution.execution.executionState,
    queryText: props.execution.execution.queryText,
    startedAt: props.execution.execution.startedAt,
    completedAt: props.execution.execution.completedAt,
    failureCode: props.execution.execution.failureCode,
  }
})

const executionMetrics = computed(() => {
  const metrics = [
    {
      key: 'chunks',
      label: t('assistant.evidence.metrics.chunks'),
      value: props.chunkReferences.length,
    },
    {
      key: 'entities',
      label: t('assistant.evidence.metrics.entities'),
      value: props.entityReferences.length,
    },
    {
      key: 'relations',
      label: t('assistant.evidence.metrics.relations'),
      value: props.relationReferences.length,
    },
    {
      key: 'evidence',
      label: t('assistant.evidence.metrics.evidence'),
      value: props.evidenceReferences.length,
    },
  ]
  const nonZeroMetrics = metrics.filter((metric) => metric.value > 0)
  return nonZeroMetrics.length > 0 ? nonZeroMetrics : metrics
})

const canOpenDocuments = computed(() => props.chunkReferences.length > 0)
const canOpenGraph = computed(
  () => props.entityReferences.length > 0 || props.relationReferences.length > 0,
)

const topChunkReferences = computed(() =>
  topReferences('chunk', props.chunkReferences, (item) => item.chunkId),
)
const topEntityReferences = computed(() =>
  topReferences('entity', props.entityReferences, (item) => item.entityId),
)
const topRelationReferences = computed(() =>
  topReferences('relation', props.relationReferences, (item) => item.relationId),
)
const topEvidenceReferences = computed(() =>
  topReferences('evidence', props.evidenceReferences, (item) => item.evidenceId),
)

const referenceSections = computed(() => [
  {
    key: 'chunks',
    title: t('assistant.evidence.sections.chunks'),
    items: topChunkReferences.value,
  },
  {
    key: 'entities',
    title: t('assistant.evidence.sections.entities'),
    items: topEntityReferences.value,
  },
  {
    key: 'relations',
    title: t('assistant.evidence.sections.relations'),
    items: topRelationReferences.value,
  },
  {
    key: 'evidence',
    title: t('assistant.evidence.sections.evidence'),
    items: topEvidenceReferences.value,
  },
].filter((section) => section.items.length > 0))
</script>

<template>
  <aside class="rr-assistant-evidence">
    <div class="rr-assistant-evidence__panel">
      <div class="rr-assistant-evidence__head">
        <div class="rr-assistant-evidence__copy">
          <span>{{ t('assistant.evidence.eyebrow') }}</span>
          <h3>{{ t('assistant.evidence.title') }}</h3>
          <p>{{ t('assistant.evidence.subtitle', { library: libraryName }) }}</p>
        </div>
        <button
          v-if="closable"
          type="button"
          class="rr-assistant-evidence__close"
          :aria-label="t('dialogs.close')"
          :title="t('dialogs.close')"
          @click="emit('close')"
        >
          <svg viewBox="0 0 14 14" aria-hidden="true">
            <path
              d="M3 3l8 8M11 3 3 11"
              fill="none"
              stroke="currentColor"
              stroke-linecap="round"
              stroke-width="1.6"
            />
          </svg>
        </button>
      </div>

      <div class="rr-assistant-evidence__body">
        <div
          v-if="error"
          class="rr-assistant-evidence__feedback rr-assistant-evidence__feedback--error"
        >
          <strong>{{ t('assistant.evidence.errorTitle') }}</strong>
          <p>{{ error }}</p>
        </div>

        <div
          v-else-if="!executionSummary"
          class="rr-assistant-evidence__empty"
        >
          <strong>{{ t('assistant.evidence.emptyTitle') }}</strong>
          <p>{{ t('assistant.evidence.emptyBody') }}</p>
          <ul>
            <li>{{ t('assistant.evidence.emptyPromptDocuments') }}</li>
            <li>{{ t('assistant.evidence.emptyPromptGraph') }}</li>
            <li>{{ t('assistant.evidence.emptyPromptCompare') }}</li>
          </ul>
        </div>

        <template v-else>
          <div
            class="rr-assistant-evidence__feedback"
            :class="{ 'rr-assistant-evidence__feedback--busy': executing }"
          >
            <strong>
              {{
                executing
                  ? t('assistant.evidence.executingTitle')
                  : t('assistant.evidence.executionTitle')
              }}
            </strong>
            <div class="rr-assistant-evidence__meta">
              <span>{{ t('assistant.evidence.executionState', { state: localizedExecutionState(executionSummary.state) }) }}</span>
              <span>{{ t('assistant.evidence.startedAt', { value: formatDateTime(executionSummary.startedAt) }) }}</span>
              <span v-if="executionSummary.completedAt">
                {{ t('assistant.evidence.completedAt', { value: formatDateTime(executionSummary.completedAt) }) }}
              </span>
              <span v-if="executionSummary.failureCode">
                {{ t('assistant.evidence.failureCode', { value: executionSummary.failureCode }) }}
              </span>
            </div>
          </div>

          <div class="rr-assistant-evidence__metrics">
            <div
              v-for="metric in executionMetrics"
              :key="metric.key"
              class="rr-assistant-evidence__metric"
            >
              <strong>{{ metric.value }}</strong>
              <span>{{ metric.label }}</span>
            </div>
          </div>

          <div
            v-if="bundle"
            class="rr-assistant-evidence__bundle"
          >
            <span>{{ t('assistant.evidence.bundleLabel') }}</span>
            <strong>{{ shortIdentifier(bundle.bundle.bundleId, 12) }}</strong>
            <p>{{ localizedBundleState(bundle.bundle.bundleState) }}</p>
          </div>

          <div
            v-for="section in referenceSections"
            :key="section.key"
            class="rr-assistant-evidence__section"
          >
            <div class="rr-assistant-evidence__section-head">
              <strong>{{ section.title }}</strong>
              <span>{{ section.items.length }}</span>
            </div>
            <ul class="rr-assistant-evidence__reference-list">
              <li
                v-for="item in section.items"
                :key="item.id"
                class="rr-assistant-evidence__reference"
              >
                <div>
                  <strong>{{ item.label }}</strong>
                  <span>#{{ item.rank }}</span>
                </div>
                <span>{{ item.score.toFixed(3) }}</span>
              </li>
            </ul>
          </div>

          <div class="rr-assistant-evidence__actions">
            <button
              v-if="canOpenDocuments"
              type="button"
              class="rr-button rr-button--secondary rr-button--compact"
              @click="emit('open-documents')"
            >
              {{ t('assistant.actions.openDocuments') }}
            </button>
            <button
              v-if="canOpenGraph"
              type="button"
              class="rr-button rr-button--ghost rr-button--compact"
              @click="emit('open-graph')"
            >
              {{ t('assistant.actions.openGraph') }}
            </button>
          </div>
        </template>
      </div>
    </div>
  </aside>
</template>
