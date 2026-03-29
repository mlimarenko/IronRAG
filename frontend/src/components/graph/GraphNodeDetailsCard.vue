<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { useDisplayFormatters } from 'src/composables/useDisplayFormatters'
import type { GraphNodeDetail } from 'src/models/ui/graph'

const props = defineProps<{
  detail: GraphNodeDetail | null
  loading: boolean
  error?: string | null
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

const canonicalSummary = computed(() => props.detail?.canonicalSummary ?? null)
const displayTitle = computed(() => {
  if (!props.detail) {
    return ''
  }

  const summary = props.detail.summary.trim()
  if (props.detail.nodeType === 'document' && summary && summary !== props.detail.label) {
    return summary
  }

  return props.detail.label
})

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

const visibleRelatedDocuments = computed(() => props.detail?.relatedDocuments.slice(0, 4) ?? [])

const visibleConnectedNodes = computed(() => {
  if (!props.detail) {
    return []
  }

  const relatedDocumentIds = new Set(props.detail.relatedDocuments.map((item) => item.id))
  return props.detail.connectedNodes.filter((item) => !relatedDocumentIds.has(item.id)).slice(0, 4)
})

const visibleLinks = computed(() => props.detail?.relatedEdges.slice(0, 5) ?? [])
const evidenceCount = computed(() => props.detail?.evidence.length ?? 0)
const relatedDocumentCount = computed(() => props.detail?.relatedDocuments.length ?? 0)

function isLowSignalMetadataValue(value: string): boolean {
  const normalized = value.trim().toLowerCase()
  return !normalized || normalized === '—' || normalized === 'unknown' || normalized === 'none' || normalized === 'n/a'
}

function isUuidLike(value: string): boolean {
  return /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(value.trim())
}

const visibleProperties = computed(() =>
  (props.detail?.properties ?? [])
    .filter(([key]) => !['Projection', 'Canonical key'].includes(key))
    .map(([key, value]) => ({
      rawKey: key,
      label: graphPropertyLabel(key),
      value: graphPropertyValue(key, value),
    })),
)

const meaningfulProperties = computed(() =>
  visibleProperties.value.filter(({ rawKey, value }) => {
    if (rawKey === 'Type') {
      return false
    }
    if (rawKey === 'Assertion' && value.trim() === displayTitle.value.trim()) {
      return false
    }
    if (
      rawKey === 'State' &&
      ['active', 'current', 'clean', 'ready'].includes(value.trim().toLowerCase())
    ) {
      return false
    }
    if (isLowSignalMetadataValue(value)) {
      return false
    }
    if (
      props.detail?.nodeType === 'document' &&
      ['External key', 'Active revision', 'Readable revision'].includes(rawKey) &&
      isUuidLike(value)
    ) {
      return false
    }
    return true
  }).slice(0, 6),
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

const heroSummary = computed(() => {
  if (!props.detail) {
    return ''
  }

  const canonicalText = canonicalSummary.value?.text.trim() ?? ''
  if (canonicalText && canonicalText !== props.detail.label) {
    return canonicalText
  }

  if (primarySummary.value) {
    return primarySummary.value
  }

  if (canonicalText) {
    return canonicalText
  }

  return t('graph.nodeSummaries.connected', { count: props.detail.relationCount })
})
const showHeroSummary = computed(
  () => heroSummary.value.trim().length > 0 && heroSummary.value.trim() !== displayTitle.value.trim(),
)
const compactHero = computed(() => !showHeroSummary.value && !heroWarning.value)

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
  if (props.detail.extractionRecovery && props.detail.extractionRecovery.status !== 'clean') {
    lines.push(t(`graph.extractionRecovery.${props.detail.extractionRecovery.status}`))
  }
  return lines
})

const heroWarning = computed(() => {
  if (!canonicalSummary.value?.warning) {
    return null
  }
  return graphWarningLabel(canonicalSummary.value.warning)
})

const heroMetaBadges = computed(() => {
  if (!canonicalSummary.value) {
    return []
  }

  const badges = [
    t('graph.summary.confidenceLine', {
      value: summaryConfidenceLabel(canonicalSummary.value.confidenceStatus),
    }),
  ]

  if (canonicalSummary.value.supportCount > 0) {
    badges.push(t('graph.summary.supportCount', { count: canonicalSummary.value.supportCount }))
  }

  return badges
})

const showHeroBlock = computed(
  () => showHeroSummary.value || Boolean(heroWarning.value) || heroMetaBadges.value.length > 0,
)

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
    .replace(/[\w-]*\);">/g, ' ')
    .replace(/">/g, ' ')
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
    const warning = graphWarningLabel(props.detail.warning)
    if (warning) {
      lines.push(warning)
    }
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
      const fallbackReason = graphWarningLabel(scope.fallbackReason)
      if (fallbackReason) {
        lines.push(fallbackReason)
      }
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

const stateSummaryLines = computed(() => {
  const lines = [
    ...(props.error ? [props.error] : []),
    ...reconciliationSummary.value,
    ...graphQualitySummary.value,
  ]

  return [...new Set(lines.filter((line) => line.trim().length > 0))]
})

const navigationSections = computed(() => {
  const sections: Array<
    | {
        key: 'documents' | 'nodes'
        kind: 'chips'
        title: string
        items: Array<{ id: string; label: string }>
      }
    | {
        key: 'edges'
        kind: 'edges'
        title: string
        items: Array<{
          id: string
          otherNodeId: string
          otherNodeLabel: string
          relationType: string
        }>
      }
  > = []

  if (visibleRelatedDocuments.value.length) {
    sections.push({
      key: 'documents',
      kind: 'chips',
      title: t('graph.relatedDocuments'),
      items: visibleRelatedDocuments.value,
    })
  }

  if (visibleConnectedNodes.value.length) {
    sections.push({
      key: 'nodes',
      kind: 'chips',
      title: t('graph.connectedNodes'),
      items: visibleConnectedNodes.value,
    })
  }

  if (visibleLinks.value.length) {
    sections.push({
      key: 'edges',
      kind: 'edges',
      title: t('graph.relatedEdges'),
      items: visibleLinks.value,
    })
  }

  return sections
})

const showNavigation = computed(() => navigationSections.value.length > 0)
const showNavigationTitles = computed(() => navigationSections.value.length > 1)
const showStateSectionTitle = computed(
  () => stateSummaryLines.value.length > 1 || visibleEvidence.value.length > 0 || showNavigation.value || meaningfulProperties.value.length > 0,
)

const headerCounters = computed(() => {
  const counters: string[] = []
  if (relatedDocumentCount.value > 0) {
    counters.push(t('graph.relatedDocumentsCount', { count: relatedDocumentCount.value }))
  }
  const summarySupportCount = canonicalSummary.value?.supportCount ?? 0
  if (evidenceCount.value > 0 && evidenceCount.value !== summarySupportCount) {
    counters.push(graphEvidenceLabel(evidenceCount.value))
  }
  return counters
})
</script>

<template>
  <section class="nc">
    <div
      v-if="props.loading"
      class="nc__loader"
    >
      <div class="nc__loader-spinner" />
      <span>{{ $t('graph.loadingNode') }}</span>
    </div>

    <div
      v-else-if="props.error && !props.detail"
      class="nc__error"
    >
      <strong>{{ $t('graph.inspectorError') }}</strong>
      <p>{{ props.error }}</p>
    </div>

    <template v-else-if="props.detail">
      <header class="nc__header">
        <div class="nc__badges">
          <span class="nc__badge nc__badge--type">
            {{ graphNodeKindLabel(props.detail.nodeType) }}
          </span>
          <span
            v-if="props.detail.relationCount > 0"
            class="nc__badge nc__badge--metric"
          >
            {{ $t('graph.relationCount', { count: props.detail.relationCount }) }}
          </span>
          <span
            v-if="props.detail.convergenceStatus"
            class="nc__badge nc__badge--status"
          >
            {{ $t(`graph.convergence.${props.detail.convergenceStatus}`) }}
          </span>
        </div>

        <h3 class="nc__title">{{ displayTitle }}</h3>

        <div
          v-if="showHeroBlock"
          class="nc__hero"
          :class="{ 'is-compact': compactHero }"
        >
          <p class="nc__eyebrow">{{ $t('graph.inspector.whyItMatters') }}</p>
          <p
            v-if="showHeroSummary"
            class="nc__hero-summary"
          >
            {{ heroSummary }}
          </p>

          <div
            v-if="heroMetaBadges.length"
            class="nc__hero-meta"
          >
            <span
              v-for="badge in heroMetaBadges"
              :key="badge"
              class="nc__badge nc__badge--metric"
            >
              {{ badge }}
            </span>
          </div>

          <p
            v-if="heroWarning"
            class="nc__hero-warning"
          >
            {{ heroWarning }}
          </p>
        </div>

        <div
          v-if="headerCounters.length"
          class="nc__counters"
        >
          <template
            v-for="(counter, index) in headerCounters"
            :key="counter"
          >
            <span>{{ counter }}</span>
            <span
              v-if="index < headerCounters.length - 1"
              class="nc__dot"
            />
          </template>
        </div>
      </header>

      <section
        v-if="stateSummaryLines.length"
        class="nc__section"
        :class="{ 'is-compact': !showStateSectionTitle }"
      >
        <div class="nc__section-head">
          <h4
            v-if="showStateSectionTitle"
            class="nc__section-title"
          >
            {{ $t('graph.inspector.graphState') }}
          </h4>
        </div>
        <div class="nc__state-list">
          <p
            v-for="(line, idx) in stateSummaryLines"
            :key="`state-${idx}`"
            class="nc__state-line"
          >
            {{ line }}
          </p>
        </div>
      </section>

      <section
        v-if="visibleEvidence.length"
        class="nc__section"
      >
        <div class="nc__section-head">
          <h4 class="nc__section-title">{{ $t('graph.inspector.evidencePreview') }}</h4>
          <span class="nc__section-caption">{{ graphEvidenceLabel(evidenceCount) }}</span>
        </div>

        <ul class="nc__evidence-list">
          <li
            v-for="item in visibleEvidence"
            :key="item.id"
            class="nc__evidence-item"
          >
            <div class="nc__evidence-item-meta">
              <span v-if="props.detail.nodeType !== 'document'">
                {{ item.documentLabel ?? $t('graph.unknownDocument') }}
              </span>
              <span v-if="item.pageRef">{{ item.pageRef }}</span>
            </div>
            <p>{{ item.evidenceText }}</p>
          </li>
        </ul>
      </section>

      <section
        v-if="showNavigation"
        class="nc__section"
      >
        <div class="nc__section-head">
          <h4 class="nc__section-title">{{ $t('graph.inspector.jumpTo') }}</h4>
        </div>

        <div class="nc__nav-groups">
          <div
            v-for="section in navigationSections"
            :key="section.key"
            class="nc__subsection"
          >
            <h5
              v-if="showNavigationTitles"
              class="nc__subsection-title"
            >
              {{ section.title }}
            </h5>

            <div
              v-if="section.kind === 'chips'"
              class="nc__chips"
            >
              <button
                v-for="item in section.items"
                :key="item.id"
                type="button"
                class="nc__chip"
                @click="$emit('selectNode', item.id)"
              >
                {{ item.label }}
              </button>
            </div>

            <ul
              v-else
              class="nc__edges"
            >
              <li
                v-for="edge in section.items"
                :key="edge.id"
                class="nc__edge"
              >
                <button
                  type="button"
                  class="nc__edge-link"
                  @click="$emit('selectNode', edge.otherNodeId)"
                >
                  {{ edge.otherNodeLabel }}
                </button>
                <span class="nc__edge-type">{{ relationLabel(edge.relationType) }}</span>
              </li>
            </ul>
          </div>
        </div>
      </section>

      <section
        v-if="meaningfulProperties.length"
        class="nc__section"
      >
        <div class="nc__section-head">
          <h4 class="nc__section-title">{{ $t('graph.inspector.metadata') }}</h4>
        </div>

        <dl class="nc__props">
          <div
            v-for="property in meaningfulProperties"
            :key="property.rawKey"
            class="nc__prop"
          >
            <dt>{{ property.label }}</dt>
            <dd>{{ property.value }}</dd>
          </div>
        </dl>
      </section>
    </template>

    <div
      v-else
      class="nc__empty"
    >
      {{ $t('graph.selectNodeHint') }}
    </div>
  </section>
</template>

<style scoped>
.nc {
  display: flex;
  flex-direction: column;
  gap: 0;
  padding: 0;
  overflow-y: auto;
  overflow-x: hidden;
  font-size: 13px;
  color: var(--rr-text-primary);
  scrollbar-width: thin;
}

.nc__loader,
.nc__error,
.nc__empty {
  display: grid;
  gap: 10px;
  padding: 24px 20px;
}

.nc__loader {
  grid-auto-flow: column;
  align-items: center;
  justify-content: start;
  color: var(--rr-text-muted);
  font-size: 13px;
}

.nc__error {
  color: rgba(146, 64, 14, 0.92);
}

.nc__error p,
.nc__empty {
  margin: 0;
}

.nc__empty {
  color: var(--rr-text-secondary);
}

.nc__loader-spinner {
  width: 16px;
  height: 16px;
  border: 2px solid rgba(99, 102, 241, 0.2);
  border-top-color: rgba(99, 102, 241, 0.8);
  border-radius: 50%;
  animation: nc-spin 0.6s linear infinite;
}

@keyframes nc-spin {
  to {
    transform: rotate(360deg);
  }
}

.nc__header {
  display: grid;
  gap: 7px;
  padding: 14px 16px 10px;
  border-bottom: 1px solid var(--rr-border-soft);
  background:
    linear-gradient(180deg, rgba(248, 250, 252, 0.95), rgba(255, 255, 255, 0.98)),
    rgba(255, 255, 255, 0.98);
}

.nc__badges,
.nc__hero-meta,
.nc__counters,
.nc__section-head {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 6px;
}

.nc__badge {
  display: inline-flex;
  align-items: center;
  min-height: 22px;
  padding: 0 9px;
  border-radius: 999px;
  font-size: 11px;
  font-weight: 600;
  letter-spacing: 0.02em;
  line-height: 1.1;
  white-space: nowrap;
}

.nc__badge--type {
  background: rgba(99, 102, 241, 0.1);
  color: var(--rr-accent);
}

.nc__badge--metric {
  background: var(--rr-border-soft);
  color: var(--rr-text-secondary);
}

.nc__badge--status {
  background: rgba(16, 185, 129, 0.1);
  color: rgba(5, 150, 105, 0.9);
}

.nc__title {
  margin: 0;
  font-size: 18px;
  font-weight: 700;
  line-height: 1.22;
  color: var(--rr-text-primary);
  word-break: break-word;
}

.nc__hero {
  display: grid;
  gap: 5px;
  padding: 9px 11px;
  border: 1px solid rgba(99, 102, 241, 0.12);
  border-radius: 14px;
  background:
    linear-gradient(135deg, rgba(99, 102, 241, 0.07), rgba(59, 130, 246, 0.04)),
    rgba(255, 255, 255, 0.96);
}

.nc__hero.is-compact {
  gap: 4px;
  padding: 8px 10px;
}

.nc__eyebrow {
  margin: 0;
  font-size: 11px;
  font-weight: 700;
  letter-spacing: 0.05em;
  text-transform: uppercase;
  color: var(--rr-text-muted);
}

.nc__hero-summary {
  margin: 0;
  color: var(--rr-text-primary);
  font-size: 13px;
  line-height: 1.48;
}

.nc__hero-warning {
  margin: 0;
  color: rgba(146, 64, 14, 0.9);
  font-size: 12px;
  line-height: 1.5;
}

.nc__counters {
  color: var(--rr-text-muted);
  font-size: 11.5px;
  font-weight: 600;
  letter-spacing: 0.01em;
}

.nc__dot {
  width: 3px;
  height: 3px;
  border-radius: 50%;
  background: rgba(148, 163, 184, 0.5);
}

.nc__section {
  display: grid;
  gap: 8px;
  padding: 10px 16px;
  border-bottom: 1px solid var(--rr-border-soft);
}

.nc__section.is-compact {
  gap: 6px;
}

.nc__section-title,
.nc__subsection-title {
  margin: 0;
  font-size: 11.5px;
  font-weight: 700;
  text-transform: uppercase;
  letter-spacing: 0.05em;
  color: var(--rr-text-muted);
}

.nc__section-caption {
  color: var(--rr-text-muted);
  font-size: 11.5px;
  font-weight: 600;
}

.nc__state-list,
.nc__nav-groups {
  display: grid;
  gap: 7px;
}

.nc__state-line {
  margin: 0;
  padding: 9px 11px;
  border-radius: 12px;
  background: rgba(248, 250, 252, 0.96);
  border: 1px solid rgba(148, 163, 184, 0.14);
  color: var(--rr-text-secondary);
  font-size: 12.5px;
  line-height: 1.5;
}

.nc__section.is-compact .nc__state-line {
  padding: 8px 10px;
}

.nc__subsection {
  display: grid;
  gap: 8px;
}

.nc__chips {
  display: flex;
  flex-wrap: wrap;
  gap: 5px;
}

.nc__chip {
  display: inline-flex;
  align-items: center;
  min-height: 28px;
  max-width: 100%;
  padding: 0 10px;
  border: 1px solid rgba(99, 102, 241, 0.15);
  border-radius: 10px;
  background: rgba(99, 102, 241, 0.05);
  color: var(--rr-accent);
  font-size: 12px;
  font-weight: 500;
  cursor: pointer;
  transition: background 120ms, border-color 120ms;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.nc__chip:hover,
.nc__chip:focus-visible {
  background: rgba(99, 102, 241, 0.12);
  border-color: rgba(99, 102, 241, 0.3);
}

.nc__edges,
.nc__evidence-list {
  display: grid;
  gap: 7px;
  margin: 0;
  padding: 0;
  list-style: none;
}

.nc__edge {
  display: flex;
  align-items: baseline;
  flex-wrap: wrap;
  gap: 8px;
}

.nc__edge-link {
  padding: 0;
  border: none;
  background: transparent;
  color: var(--rr-accent);
  font-size: 13px;
  font-weight: 600;
  cursor: pointer;
  text-align: left;
}

.nc__edge-link:hover,
.nc__edge-link:focus-visible {
  text-decoration: underline;
}

.nc__edge-type {
  color: var(--rr-text-muted);
  font-size: 11.5px;
  white-space: nowrap;
}

.nc__evidence-item {
  display: grid;
  gap: 5px;
  padding: 8px 10px;
  border-radius: 12px;
  background: rgba(248, 250, 252, 0.96);
  border: 1px solid rgba(148, 163, 184, 0.14);
}

.nc__evidence-item-meta {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 8px;
  color: var(--rr-text-muted);
  font-size: 11.5px;
  font-weight: 600;
}

.nc__evidence-item p {
  margin: 0;
  color: var(--rr-text-primary);
  font-size: 12.5px;
  line-height: 1.55;
  display: -webkit-box;
  -webkit-line-clamp: 3;
  -webkit-box-orient: vertical;
  overflow: hidden;
}

.nc__props {
  display: grid;
  gap: 7px 10px;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  margin: 0;
}

.nc__prop {
  display: grid;
  gap: 4px;
  padding: 9px 11px;
  border-radius: 12px;
  background: rgba(248, 250, 252, 0.9);
  border: 1px solid rgba(148, 163, 184, 0.12);
}

.nc__prop dt {
  color: var(--rr-text-muted);
  font-size: 11px;
  font-weight: 700;
  text-transform: uppercase;
  letter-spacing: 0.05em;
}

.nc__prop dd {
  margin: 0;
  color: var(--rr-text-primary);
  font-size: 13px;
  font-weight: 500;
  word-break: break-word;
}

@media (min-width: 1280px) {
  .nc__props {
    grid-template-columns: repeat(3, minmax(0, 1fr));
  }
}

@media (max-width: 640px) {
  .nc__props {
    grid-template-columns: 1fr;
  }
}

@media (min-width: 1800px) {
  .nc__props {
    grid-template-columns: repeat(3, minmax(0, 1fr));
  }
}
</style>
