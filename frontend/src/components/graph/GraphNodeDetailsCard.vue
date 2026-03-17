<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import type { GraphNodeDetail } from 'src/models/ui/graph'

const props = defineProps<{
  detail: GraphNodeDetail | null
  loading: boolean
}>()

defineEmits<{
  selectNode: [id: string]
}>()

const { t } = useI18n()

const visibleEvidence = computed(
  () =>
    props.detail?.evidence
      .filter((item) => item.activeProvenanceOnly)
      .slice(0, 3)
      .map((item) => ({
        ...item,
        evidenceText: normalizeEvidenceText(item.evidenceText),
      })) ?? [],
)
const visibleLinks = computed(() => props.detail?.relatedEdges.slice(0, 5) ?? [])

const visibleProperties = computed(() =>
  (props.detail?.properties ?? [])
    .filter(([key]) => !['Projection', 'Canonical key'].includes(key))
    .slice(0, 3)
    .map(([key, value]) => [propertyLabel(key), propertyValue(key, value)] as const),
)

const detailSummary = computed(() => {
  if (!props.detail) {
    return ''
  }

  if (props.detail.nodeType === 'document') {
    return t('graph.nodeSummaries.document')
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
  return lines
})

function propertyLabel(key: string): string {
  switch (key) {
    case 'Type':
      return t('graph.propertyLabels.type')
    case 'Support':
      return t('graph.propertyLabels.support')
    case 'Aliases':
      return t('graph.propertyLabels.aliases')
    case 'Source chunks':
      return t('graph.propertyLabels.sourceChunks')
    default:
      return key
    }
}

function propertyValue(key: string, value: string): string {
  if (key === 'Type') {
    if (value === 'document' || value === 'entity' || value === 'topic') {
      return t(`graph.nodeTypes.${value}`)
    }
  }
  return value
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
    requires: t('graph.relationLabels.requires'),
    includes: t('graph.relationLabels.includes'),
  }
  return mapping[normalized] ?? value.replace(/[_-]+/g, ' ')
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
  if (!props.detail?.warning) {
    return null
  }

  return [
    props.detail.warning,
    props.detail.pendingDeleteCount > 0
      ? t('graph.pendingDeleteBanner', { count: props.detail.pendingDeleteCount })
      : null,
    props.detail.pendingUpdateCount > 0
      ? t('graph.pendingUpdateBanner', { count: props.detail.pendingUpdateCount })
      : null,
  ]
    .filter(Boolean)
    .join(' ')
})
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
      <div class="rr-graph-node-card__eyebrow">
        <span class="rr-graph-node-card__type">
          {{ $t(`graph.nodeTypes.${props.detail.nodeType}`) }}
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
      <p>{{ detailSummary }}</p>

      <div
        v-if="reconciliationSummary"
        class="rr-graph-node-card__warning"
      >
        {{ reconciliationSummary }}
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

      <div
        v-if="visibleEvidence.length"
        class="rr-graph-node-card__section"
      >
        <strong>{{ $t('graph.evidence') }}</strong>
        <ul class="rr-graph-node-card__evidence">
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
      </div>
    </template>
    <p
      v-else
      class="rr-graph-node-card__empty"
    >
      {{ $t('graph.selectNodeHint') }}
    </p>
  </section>
</template>
