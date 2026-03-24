<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { useDisplayFormatters } from 'src/composables/useDisplayFormatters'
import type { GraphNodeDetail } from 'src/models/ui/graph'

const props = defineProps<{
  detail: GraphNodeDetail | null
  loading: boolean
}>()

defineEmits<{
  selectNode: [id: string]
}>()

const { t } = useI18n()
const {
  graphEvidenceLabel,
  graphNodeKindLabel,
  graphPropertyLabel,
  graphPropertyValue,
  graphWarningLabel,
  humanizeToken,
} = useDisplayFormatters()

const visibleEvidence = computed(
  () =>
    props.detail?.evidence
      .filter((item) => item.activeProvenanceOnly)
      .slice(0, 2)
      .map((item) => ({
        ...item,
        evidenceText: normalizeEvidenceText(item.evidenceText),
      })) ?? [],
)
const visibleLinks = computed(() => props.detail?.relatedEdges.slice(0, 4) ?? [])
const visibleConnectedNodes = computed(() => props.detail?.connectedNodes.slice(0, 4) ?? [])
const evidenceCount = computed(() => props.detail?.evidence.length ?? 0)
const relatedDocumentCount = computed(() => props.detail?.relatedDocuments.length ?? 0)

const visibleProperties = computed(() =>
  (props.detail?.properties ?? [])
    .filter(([key]) => !['Projection', 'Canonical key'].includes(key))
    .slice(0, 4)
    .map(([key, value]) => [graphPropertyLabel(key), graphPropertyValue(key, value)] as const),
)

const primarySummary = computed(() => {
  if (!props.detail) {
    return ''
  }

  if (props.detail.summary && props.detail.summary.trim() && props.detail.summary !== props.detail.label) {
    return props.detail.summary
  }

  if (props.detail.nodeType === 'document') {
    return ''
  }

  return t('graph.nodeSummaries.connected', { count: props.detail.relationCount })
})

const graphQualitySummary = computed(() => {
  if (!props.detail) {
    return []
  }

  const lines: string[] = []
  if (props.detail.activeProvenanceOnly) {
    lines.push(t('graph.admittedOnlyHint'))
  }
  if ((props.detail.filteredArtifactCount ?? 0) > 0) {
    lines.push(
      t('graph.filteredArtifactsHint', {
        count: props.detail.filteredArtifactCount ?? 0,
      }),
    )
  }
  if (props.detail.convergenceStatus && props.detail.convergenceStatus !== 'current') {
    lines.push(t(`graph.convergenceDescriptions.${props.detail.convergenceStatus}`))
  }
  if (
    props.detail.extractionRecovery &&
    props.detail.extractionRecovery.status !== 'clean'
  ) {
    lines.push(
      t(`graph.extractionRecovery.${props.detail.extractionRecovery.status}`),
    )
  }
  return lines
})

function reconciliationScopeLabel(status: string, count: number): string {
  return t(`graph.reconciliation.scope.${status}`, { count })
}

function reconciliationConfidenceLabel(status: string): string {
  return t(`graph.reconciliation.confidence.${status}`)
}

function summaryConfidenceLabel(status: string): string {
  return t(`graph.summary.confidence.${status}`)
}

function relationLabel(value: string): string {
  const normalized = value.trim().toLowerCase().replace(/[\s-]+/g, '_')
  const mapping: Record<string, string> = {
    mentions: t('graph.relationLabels.mentions'),
    uses: t('graph.relationLabels.uses'),
    maintains: t('graph.relationLabels.maintains'),
    works_with: t('graph.relationLabels.worksWith'),
    workswith: t('graph.relationLabels.worksWith'),
    related_to: t('graph.relationLabels.relatedTo'),
    led_by: t('graph.relationLabels.ledBy'),
    owned_by: t('graph.relationLabels.ownedBy'),
    document_reference: t('graph.relationLabels.documentReference'),
    configured_by: t('graph.relationLabels.configuredBy'),
    auth_method: t('graph.relationLabels.authMethod'),
    deployed_via: t('graph.relationLabels.deployedVia'),
    serves_static_for: t('graph.relationLabels.servesStaticFor'),
    delegates_auth_callbacks_to: t('graph.relationLabels.delegatesAuthCallbacksTo'),
    subject: t('graph.relationLabels.subject'),
    supported_by: t('graph.relationLabels.supportedBy'),
    requires: t('graph.relationLabels.requires'),
    includes: t('graph.relationLabels.includes'),
  }
  return mapping[normalized] ?? humanizeToken(value)
}

function normalizeEvidenceText(value: string): string {
  const normalized = value
    .replace(/<[^>]+>/g, ' ')
    .replace(/&nbsp;/gi, ' ')
    .replace(/&[a-z]+;/gi, ' ')
    .replace(/\s+/g, ' ')
    .trim()

  return normalized || value
}

const reconciliationSummary = computed(() => {
  if (!props.detail) {
    return []
  }

  const lines: string[] = []
  if (props.detail.warning) {
    lines.push(graphWarningLabel(props.detail.warning) ?? props.detail.warning)
  }
  const scope = props.detail.reconciliationScope
  if (scope) {
    lines.push(
      reconciliationScopeLabel(
        scope.scopeStatus,
        scope.affectedNodeCount + scope.affectedRelationshipCount,
      ),
    )
    lines.push(
      t('graph.reconciliation.confidenceLine', {
        value: reconciliationConfidenceLabel(scope.confidenceStatus),
      }),
    )
    if (scope.fallbackReason) {
      lines.push(graphWarningLabel(scope.fallbackReason) ?? scope.fallbackReason)
    }
  }
  if (props.detail.pendingDeleteCount > 0) {
    lines.push(t('graph.pendingDeleteBanner', { count: props.detail.pendingDeleteCount }))
  }
  if (props.detail.pendingUpdateCount > 0) {
    lines.push(t('graph.pendingUpdateBanner', { count: props.detail.pendingUpdateCount }))
  }
  return lines
})

const canonicalSummary = computed(() => props.detail?.canonicalSummary ?? null)
</script>

<template>
  <section class="rr-graph-node-card">
    <p
      v-if="props.loading"
      class="rr-graph-node-card__loading"
    >
      {{ $t('graph.loadingNode') }}
    </p>
    <template v-else-if="props.detail">
      <header class="rr-graph-node-card__head">
        <div class="rr-graph-node-card__eyebrow">
          <span class="rr-graph-node-card__type">
            {{ graphNodeKindLabel(props.detail.nodeType) }}
          </span>
          <span class="rr-graph-node-card__metric">
            {{ $t('graph.relationCount', { count: props.detail.relationCount }) }}
          </span>
          <span
            v-if="props.detail.convergenceStatus"
            class="rr-graph-node-card__metric"
          >
            {{ $t(`graph.convergence.${props.detail.convergenceStatus}`) }}
          </span>
        </div>

        <h3>{{ props.detail.label }}</h3>
        <p v-if="primarySummary">{{ primarySummary }}</p>
        <div class="rr-graph-node-card__stats">
          <span>{{ $t('graph.relatedDocumentsCount', { count: relatedDocumentCount }) }}</span>
          <span>{{ graphEvidenceLabel(evidenceCount) }}</span>
        </div>
      </header>

      <dl class="rr-graph-node-card__properties">
        <div
          v-for="property in visibleProperties"
          :key="property[0]"
        >
          <dt>{{ property[0] }}</dt>
          <dd>{{ property[1] }}</dd>
        </div>
      </dl>

      <div
        v-if="props.detail.relatedDocuments.length"
        class="rr-graph-node-card__section"
      >
        <strong>{{ $t('graph.relatedDocuments') }}</strong>
        <div class="rr-graph-node-card__chips">
          <button
            v-for="item in props.detail.relatedDocuments.slice(0, 4)"
            :key="item.id"
            type="button"
            class="rr-graph-node-card__chip"
            @click="$emit('selectNode', item.id)"
          >
            {{ item.label }}
          </button>
        </div>
      </div>

      <div
        v-if="visibleConnectedNodes.length"
        class="rr-graph-node-card__section"
      >
        <strong>{{ $t('graph.connectedNodes') }}</strong>
        <div class="rr-graph-node-card__chips">
          <button
            v-for="item in visibleConnectedNodes"
            :key="item.id"
            type="button"
            class="rr-graph-node-card__chip"
            @click="$emit('selectNode', item.id)"
          >
            {{ item.label }}
          </button>
        </div>
      </div>

      <div
        v-if="visibleLinks.length"
        class="rr-graph-node-card__section"
      >
        <strong>{{ $t('graph.relatedEdges') }}</strong>
        <ul class="rr-graph-node-card__list">
          <li
            v-for="edge in visibleLinks"
            :key="edge.id"
          >
            <button
              type="button"
              class="rr-graph-node-card__link"
              @click="$emit('selectNode', edge.otherNodeId)"
            >
              {{ edge.otherNodeLabel }}
            </button>
            <span>{{ relationLabel(edge.relationType) }}</span>
          </li>
        </ul>
      </div>

      <details
        v-if="canonicalSummary || reconciliationSummary.length || graphQualitySummary.length || visibleEvidence.length"
        class="rr-graph-node-card__disclosure"
      >
        <summary>{{ $t('graph.evidence') }}</summary>

        <div
          v-if="canonicalSummary"
          class="rr-graph-node-card__summary"
        >
          <div class="rr-graph-node-card__summary-meta">
            <strong>{{ $t('graph.summary.title') }}</strong>
            <span class="rr-graph-node-card__metric">
              {{
                $t('graph.summary.confidenceLine', {
                  value: summaryConfidenceLabel(canonicalSummary.confidenceStatus),
                })
              }}
            </span>
            <span class="rr-graph-node-card__metric">
              {{
                $t('graph.summary.supportCount', {
                  count: canonicalSummary.supportCount,
                })
              }}
            </span>
          </div>
          <p>{{ canonicalSummary.text }}</p>
          <p
            v-if="canonicalSummary.warning"
            class="rr-graph-node-card__summary-warning"
          >
            {{ graphWarningLabel(canonicalSummary.warning) ?? canonicalSummary.warning }}
          </p>
        </div>

        <div
          v-if="reconciliationSummary.length"
          class="rr-graph-node-card__warning"
        >
          <p
            v-for="line in reconciliationSummary"
            :key="line"
          >
            {{ line }}
          </p>
        </div>

        <div
          v-if="graphQualitySummary.length"
          class="rr-graph-node-card__note"
        >
          <p
            v-for="line in graphQualitySummary"
            :key="line"
          >
            {{ line }}
          </p>
        </div>

        <ul
          v-if="visibleEvidence.length"
          class="rr-graph-node-card__evidence"
        >
          <li
            v-for="item in visibleEvidence"
            :key="item.id"
          >
            <div class="rr-graph-node-card__evidence-meta">
              <span>{{ item.documentLabel ?? $t('graph.unknownDocument') }}</span>
              <span v-if="item.pageRef">{{ item.pageRef }}</span>
            </div>
            <p>{{ item.evidenceText }}</p>
          </li>
        </ul>
      </details>
    </template>
    <p
      v-else
      class="rr-graph-node-card__empty"
    >
      {{ $t('graph.selectNodeHint') }}
    </p>
  </section>
</template>
