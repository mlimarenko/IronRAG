<script setup lang="ts">
import { computed } from 'vue'
import type {
  KnowledgeBundleEntityReference,
  KnowledgeBundleRelationReference,
  KnowledgeContextBundleDetail,
  QueryExecutionDetail,
  RuntimePolicyDecisionSummary,
} from 'src/services/api/query'
import { useDisplayFormatters } from 'src/composables/useDisplayFormatters'
import { useI18n } from 'vue-i18n'

interface EvidenceCardItem {
  id: string
  title: string
  meta: string | null
  badges: string[]
  rank: number
  score: number
}

const props = withDefaults(
  defineProps<{
    libraryName: string
    executing: boolean
    error: string | null
    execution: QueryExecutionDetail | null
    bundle: KnowledgeContextBundleDetail | null
    closable?: boolean
  }>(),
  {
    closable: false,
  },
)

const emit = defineEmits<(event: 'open-graph' | 'open-documents' | 'close') => void>()

const { t } = useI18n()
const { formatDateTime, shortIdentifier } = useDisplayFormatters()

function humanizeToken(value: string): string {
  return value.replace(/[_-]+/g, ' ').replace(/\s+/g, ' ').trim()
}

function localizedLifecycleState(state: string): string {
  const key = `assistant.evidence.executionStates.${state}`
  const translated = t(key)
  return translated === key ? humanizeToken(state) : translated
}

function localizedRuntimeStage(stage: string, fallbackLabel?: string | null): string {
  if (fallbackLabel) {
    return fallbackLabel
  }
  const key = `assistant.evidence.runtimeStages.${stage}`
  const translated = t(key)
  return translated === key ? humanizeToken(stage) : translated
}

function localizedBundleState(state: string): string {
  const key = `assistant.evidence.bundleStates.${state}`
  const translated = t(key)
  return translated === key ? humanizeToken(state) : translated
}

function localizedPolicyDecisionKind(decisionKind: string): string {
  const key = `assistant.evidence.policyDecisionKinds.${decisionKind}`
  const translated = t(key)
  return translated === key ? humanizeToken(decisionKind) : translated
}

function localizedPolicyTargetKind(targetKind: string): string {
  const key = `assistant.evidence.policyTargetKinds.${targetKind}`
  const translated = t(key)
  return translated === key ? humanizeToken(targetKind) : translated
}

function localizedBlockKind(kind: string): string {
  const key = `assistant.evidence.segmentKinds.${kind}`
  const translated = t(key)
  return translated === key ? humanizeToken(kind) : translated
}

function localizedFactKind(kind: string): string {
  const key = `assistant.evidence.factKinds.${kind}`
  const translated = t(key)
  return translated === key ? humanizeToken(kind) : translated
}

function localizedInclusionReason(reason: string | null): string | null {
  if (!reason) {
    return null
  }
  const key = `assistant.evidence.inclusionReasons.${reason}`
  const translated = t(key)
  return translated === key ? humanizeToken(reason) : translated
}

function joinReferencePath(parts: string[]): string | null {
  const filtered = parts.map((part) => part.trim()).filter(Boolean)
  return filtered.length > 0 ? filtered.join(' > ') : null
}

function dedupeRankedItems<T extends { rank: number; score: number }>(
  items: T[],
  resolveId: (item: T) => string,
): T[] {
  const deduped = new Map<string, T>()
  for (const item of items) {
    const id = resolveId(item)
    const existing = deduped.get(id)
    if (
      !existing ||
      item.rank < existing.rank ||
      (item.rank === existing.rank && item.score > existing.score)
    ) {
      deduped.set(id, item)
    }
  }
  return Array.from(deduped.values()).sort(
    (left, right) => left.rank - right.rank || right.score - left.score,
  )
}

const executionSummary = computed(() => {
  if (!props.execution) {
    return null
  }

  return {
    state: props.execution.runtimeSummary.lifecycleState,
    activeStage: props.execution.runtimeSummary.activeStage,
    stageSummaries: props.execution.runtimeStageSummaries,
    policySummary: props.execution.runtimeSummary.policySummary,
    queryText: props.execution.execution.queryText,
    startedAt: props.execution.runtimeSummary.acceptedAt,
    completedAt: props.execution.runtimeSummary.completedAt,
    failureCode:
      props.execution.runtimeSummary.failureCode ?? props.execution.execution.failureCode,
  }
})

const activeStageLabel = computed(() => {
  const summary = executionSummary.value
  if (!summary?.activeStage) {
    return null
  }
  const stage = summary.stageSummaries.find((item) => item.stageKind === summary.activeStage)
  return localizedRuntimeStage(summary.activeStage, stage?.stageLabel ?? null)
})

const runtimeStageHistory = computed(() => {
  if (!props.execution) {
    return []
  }
  return props.execution.runtimeStageSummaries.map((item) => ({
    key: item.stageKind,
    label: localizedRuntimeStage(item.stageKind, item.stageLabel),
    active: item.stageKind === props.execution?.runtimeSummary.activeStage,
  }))
})

const policySummary = computed(() => executionSummary.value?.policySummary ?? null)

const policyInterventions = computed(() => {
  const decisions = policySummary.value?.recentDecisions ?? []
  return decisions.filter(
    (decision) => decision.decisionKind === 'reject' || decision.decisionKind === 'terminate',
  )
})

const hasPolicyIntervention = computed(
  () =>
    (policySummary.value?.rejectCount ?? 0) > 0 || (policySummary.value?.terminateCount ?? 0) > 0,
)

const policyDecisionItems = computed<
  Array<RuntimePolicyDecisionSummary & { decisionLabel: string; targetLabel: string }>
>(() =>
  policyInterventions.value.map((decision) => ({
    ...decision,
    decisionLabel: localizedPolicyDecisionKind(decision.decisionKind),
    targetLabel: localizedPolicyTargetKind(decision.targetKind),
  })),
)

const executionMetrics = computed(() => {
  if (!props.execution) {
    return []
  }
  return [
    {
      key: 'segments',
      label: t('assistant.evidence.metrics.segments'),
      value: props.execution.preparedSegmentReferences.length,
    },
    {
      key: 'facts',
      label: t('assistant.evidence.metrics.facts'),
      value: props.execution.technicalFactReferences.length,
    },
    {
      key: 'entities',
      label: t('assistant.evidence.metrics.entities'),
      value: props.execution.entityReferences.length,
    },
    {
      key: 'relations',
      label: t('assistant.evidence.metrics.relations'),
      value: props.execution.relationReferences.length,
    },
  ]
})

const canOpenDocuments = computed(() =>
  Boolean(
    props.execution &&
    (props.execution.preparedSegmentReferences.length > 0 ||
      props.execution.technicalFactReferences.length > 0 ||
      props.execution.chunkReferences.length > 0),
  ),
)

const canOpenGraph = computed(() =>
  Boolean(
    props.execution &&
    (props.execution.entityReferences.length > 0 || props.execution.relationReferences.length > 0),
  ),
)

const topPreparedSegments = computed<EvidenceCardItem[]>(() => {
  if (!props.execution) {
    return []
  }
  return dedupeRankedItems(props.execution.preparedSegmentReferences, (item) => item.segmentId)
    .slice(0, 6)
    .map((item) => {
      const headingPath = joinReferencePath(item.headingTrail)
      const sectionPath = joinReferencePath(item.sectionPath)
      return {
        id: item.segmentId,
        title: headingPath ?? sectionPath ?? shortIdentifier(item.segmentId, 12),
        meta: sectionPath && sectionPath !== headingPath ? sectionPath : null,
        badges: [localizedBlockKind(item.blockKind)],
        rank: item.rank,
        score: item.score,
      }
    })
})

const topTechnicalFacts = computed<EvidenceCardItem[]>(() => {
  if (!props.execution) {
    return []
  }
  return dedupeRankedItems(props.execution.technicalFactReferences, (item) => item.factId)
    .slice(0, 6)
    .map((item) => ({
      id: item.factId,
      title: item.displayValue,
      meta: item.canonicalValue !== item.displayValue ? item.canonicalValue : null,
      badges: [localizedFactKind(item.factKind)],
      rank: item.rank,
      score: item.score,
    }))
})

function mapGraphReferenceItems<T extends { rank: number; score: number }>(
  items: T[],
  resolveId: (item: T) => string,
  resolveReason: (item: T) => string | null,
  labelKey: 'entity' | 'relation',
): EvidenceCardItem[] {
  return dedupeRankedItems(items, resolveId)
    .slice(0, 6)
    .map((item) => {
      const id = resolveId(item)
      const reason = localizedInclusionReason(resolveReason(item))
      return {
        id,
        title: t(`assistant.evidence.labels.${labelKey}`, { id: shortIdentifier(id, 12) }),
        meta: reason,
        badges: [t(`assistant.evidence.graphKinds.${labelKey}`)],
        rank: item.rank,
        score: item.score,
      }
    })
}

const topEntityReferences = computed<EvidenceCardItem[]>(() =>
  mapGraphReferenceItems<KnowledgeBundleEntityReference>(
    props.bundle?.entityReferences ?? [],
    (item) => item.entityId,
    (item) => item.inclusionReason,
    'entity',
  ),
)

const topRelationReferences = computed<EvidenceCardItem[]>(() =>
  mapGraphReferenceItems<KnowledgeBundleRelationReference>(
    props.bundle?.relationReferences ?? [],
    (item) => item.relationId,
    (item) => item.inclusionReason,
    'relation',
  ),
)

const referenceSections = computed(() =>
  [
    {
      key: 'segments',
      title: t('assistant.evidence.sections.segments'),
      items: topPreparedSegments.value,
    },
    {
      key: 'facts',
      title: t('assistant.evidence.sections.facts'),
      items: topTechnicalFacts.value,
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
  ].filter((section) => section.items.length > 0),
)
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

        <div v-else-if="!executionSummary" class="rr-assistant-evidence__empty">
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
              <span>{{
                t('assistant.evidence.executionState', {
                  state: localizedLifecycleState(executionSummary.state),
                })
              }}</span>
              <span v-if="activeStageLabel">
                {{ t('assistant.evidence.activeStage', { stage: activeStageLabel }) }}
              </span>
              <span>{{
                t('assistant.evidence.startedAt', {
                  value: formatDateTime(executionSummary.startedAt),
                })
              }}</span>
              <span v-if="executionSummary.completedAt">
                {{
                  t('assistant.evidence.completedAt', {
                    value: formatDateTime(executionSummary.completedAt),
                  })
                }}
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

          <div v-if="runtimeStageHistory.length > 0" class="rr-assistant-evidence__runtime-stages">
            <span>{{ t('assistant.evidence.runtimeStagesLabel') }}</span>
            <div class="rr-assistant-evidence__runtime-stage-list">
              <span
                v-for="stage in runtimeStageHistory"
                :key="stage.key"
                class="rr-assistant-evidence__runtime-stage-pill"
                :class="{ 'rr-assistant-evidence__runtime-stage-pill--active': stage.active }"
              >
                {{ stage.label }}
              </span>
            </div>
          </div>

          <div
            v-if="hasPolicyIntervention"
            class="rr-assistant-evidence__policy rr-assistant-evidence__policy--intervention"
          >
            <div class="rr-assistant-evidence__policy-head">
              <span>{{ t('assistant.evidence.policyTitle') }}</span>
              <strong>
                {{
                  t('assistant.evidence.policyCounts', {
                    rejectCount: policySummary?.rejectCount ?? 0,
                    terminateCount: policySummary?.terminateCount ?? 0,
                  })
                }}
              </strong>
            </div>
            <ul class="rr-assistant-evidence__policy-list">
              <li
                v-for="decision in policyDecisionItems"
                :key="`${decision.targetKind}:${decision.decisionKind}:${decision.reasonCode}`"
              >
                <strong>{{ decision.decisionLabel }}</strong>
                <span>{{ decision.targetLabel }}</span>
                <p>{{ decision.reasonSummaryRedacted }}</p>
              </li>
            </ul>
          </div>

          <div v-if="bundle" class="rr-assistant-evidence__bundle">
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
                <div class="rr-assistant-evidence__reference-main">
                  <div class="rr-assistant-evidence__reference-head">
                    <strong>{{ item.title }}</strong>
                    <span>#{{ item.rank }}</span>
                  </div>
                  <p v-if="item.meta">{{ item.meta }}</p>
                  <div
                    v-if="item.badges.length > 0"
                    class="rr-assistant-evidence__reference-badges"
                  >
                    <span
                      v-for="badge in item.badges"
                      :key="badge"
                      class="rr-assistant-evidence__reference-badge"
                    >
                      {{ badge }}
                    </span>
                  </div>
                </div>
                <span class="rr-assistant-evidence__reference-score">
                  {{ item.score.toFixed(3) }}
                </span>
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
