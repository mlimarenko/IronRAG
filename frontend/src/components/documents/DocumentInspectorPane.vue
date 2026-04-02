<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import StatusPill from 'src/components/base/StatusPill.vue'
import DocumentPreparationStatusCard from 'src/components/documents/DocumentPreparationStatusCard.vue'
import DocumentPreparedSegmentsList from 'src/components/documents/DocumentPreparedSegmentsList.vue'
import DocumentTechnicalFactsList from 'src/components/documents/DocumentTechnicalFactsList.vue'
import { useDisplayFormatters } from 'src/composables/useDisplayFormatters'
import type { DocumentDetail, WebDiscoveredPage } from 'src/models/ui/documents'

const props = defineProps<{
  open: boolean
  detail: DocumentDetail | null
  loading: boolean
  error: string | null
  downloadingId?: string | null
  webRunCandidate?: WebDiscoveredPage | null
}>()

const emit = defineEmits<{
  close: []
  append: [id: string]
  replace: [id: string]
  retry: [id: string]
  remove: [id: string]
  openInGraph: [graphNodeId: string]
  downloadText: [id: string]
  openWebRun: [runId: string]
}>()

const { t } = useI18n()
const {
  documentMetadataLabel,
  documentReadinessLabel,
  documentStatusLabel,
  mutationKindLabel,
  formatDateTime,
  uploadFailureLabel,
} = useDisplayFormatters()
const previewExpanded = ref(false)
const failureExpanded = ref(false)
const previewCollapseThreshold = 560
const failureCollapseThreshold = 220

const statusLabel = computed(() =>
  props.detail
    ? props.detail.preparation
      ? documentReadinessLabel(props.detail.preparation.readinessKind)
      : documentStatusLabel(props.detail.status)
    : t('documents.statuses.queued'),
)

const statusTone = computed<
  | 'queued'
  | 'processing'
  | 'failed'
  | 'graph_ready'
  | 'readable'
  | 'graph_sparse'
  | 'active'
  | 'blocked'
  | 'retrying'
  | 'stalled'
>(() => {
  if (!props.detail) {
    return 'queued'
  }
  switch (props.detail.preparation?.readinessKind ?? null) {
    case 'graph_ready':
      return 'graph_ready'
    case 'readable':
      return 'readable'
    case 'graph_sparse':
      return 'graph_sparse'
    case 'failed':
      return 'failed'
    case 'processing':
      return props.detail.activityStatus === 'ready' ? 'graph_ready' : props.detail.activityStatus
    default:
      break
  }

  if (props.detail.status === 'ready_no_graph') {
    return 'graph_sparse'
  }
  if (props.detail.status === 'ready') {
    return 'graph_ready'
  }
  return props.detail.activityStatus === 'ready' ? 'graph_ready' : props.detail.activityStatus
})

const mutationLabel = computed(() => {
  const detail = props.detail
  if (!detail?.mutation.kind) {
    return null
  }
  const kindLabel = mutationKindLabel(detail.mutation.kind)
  const statusKey = detail.mutation.status
    ? `documents.mutation.status.${detail.mutation.status}`
    : null
  const statusLabelValue = statusKey && t(statusKey)
  return statusLabelValue ? `${kindLabel} · ${statusLabelValue}` : kindLabel
})

const metaLine = computed(() => {
  if (!props.detail) {
    return ''
  }
  return [
    props.detail.fileType,
    props.detail.fileSizeLabel,
    formatDateTime(props.detail.uploadedAt),
  ]
    .filter(Boolean)
    .join(' · ')
})

const previewText = computed(() => {
  const preview = props.detail?.extractedStats.previewText?.trim() ?? ''
  return preview.length > 0 ? preview : null
})

const previewIsLong = computed(() => (previewText.value?.length ?? 0) > previewCollapseThreshold)

const previewVisibleText = computed(() => {
  if (!previewText.value) {
    return null
  }
  if (previewExpanded.value || !previewIsLong.value) {
    return previewText.value
  }
  return `${previewText.value.slice(0, previewCollapseThreshold).trimEnd()}…`
})

const summaryLine = computed(() => {
  const summary = props.detail?.summary.trim() ?? ''
  if (!summary.length) {
    return null
  }
  const fileName = props.detail?.fileName.trim() ?? ''
  return summary === fileName ? null : summary
})

const preparationSummary = computed(() => props.detail?.preparation ?? null)
const preparedSegments = computed(() => props.detail?.preparedSegments ?? [])
const technicalFacts = computed(() => props.detail?.technicalFacts ?? [])

const mutationSummary = computed(() => mutationLabel.value)

const mutationWarningLabel = computed(() =>
  uploadFailureLabel(props.detail?.mutation.warning ?? null),
)
const failureMessage = computed(() => props.detail?.errorMessage ?? null)
const failureActionMessage = computed(() => props.detail?.errorActionMessage ?? null)
function isCanonicalWorkerSupersede(value: string | null): boolean {
  return (value?.trim() ?? '') === 'mutation applied by canonical worker'
}
function presentFailureText(value: string | null): string | null {
  const normalized = value?.trim() ?? ''
  if (!normalized) {
    return null
  }
  if (isCanonicalWorkerSupersede(normalized)) {
    return t('documents.details.failureCanonicalMutationApplied')
  }
  return normalized
}

const failureIsSupersededRun = computed(
  () =>
    isCanonicalWorkerSupersede(failureActionMessage.value) ||
    isCanonicalWorkerSupersede(failureMessage.value),
)

const failureTitle = computed(() =>
  failureIsSupersededRun.value
    ? t('documents.details.failureSupersededTitle')
    : t('documents.details.failureTitle'),
)

const failureSummaryText = computed(() => {
  const actionMessage = presentFailureText(failureActionMessage.value) ?? ''
  if (actionMessage.length > 0) {
    return actionMessage
  }
  const message = presentFailureText(failureMessage.value) ?? ''
  if (!message) {
    return null
  }
  if (message.length <= failureCollapseThreshold) {
    return message
  }
  return `${message.slice(0, failureCollapseThreshold).trimEnd()}…`
})
const failureDetailsText = computed(() => {
  const message = presentFailureText(failureMessage.value) ?? ''
  if (!message) {
    return null
  }
  const summary = failureSummaryText.value?.trim() ?? ''
  if (!summary) {
    return message
  }
  return summary === message ? null : message
})

const hasQuickExploreActions = computed(() =>
  Boolean(
    props.detail?.graphNodeId ?? (previewText.value !== null && props.detail?.canDownloadText),
  ),
)
const preparedSegmentsTotal = computed(
  () => preparationSummary.value?.preparedSegmentCount ?? preparedSegments.value.length,
)
const technicalFactsTotal = computed(
  () => preparationSummary.value?.technicalFactCount ?? technicalFacts.value.length,
)
const hasPreparedSegments = computed(() => preparedSegmentsTotal.value > 0)
const hasTechnicalFacts = computed(() => technicalFactsTotal.value > 0)

watch(
  () => props.detail?.id ?? null,
  () => {
    previewExpanded.value = false
    failureExpanded.value = false
  },
)

function formatCost(amount: number | null, currencyCode: string | null): string {
  if (amount === null || amount <= 0) {
    return '—'
  }
  if (amount < 0.001) {
    return currencyCode === 'USD' || !currencyCode ? '<$0.001' : `<0.001 ${currencyCode}`
  }
  const formatter = new Intl.NumberFormat(undefined, {
    style: 'currency',
    currency: currencyCode ?? 'USD',
    minimumFractionDigits: amount < 0.01 ? 4 : 2,
    maximumFractionDigits: amount < 0.01 ? 4 : 3,
  })
  return formatter.format(amount)
}

const overviewRows = computed(() => {
  const detail = props.detail
  if (!detail) {
    return []
  }
  return [
    {
      key: 'uploaded',
      label: documentMetadataLabel('uploaded'),
      value: formatDateTime(detail.uploadedAt),
    },
    {
      key: 'status',
      label: documentMetadataLabel('status'),
      value: detail.preparation
        ? documentReadinessLabel(detail.preparation.readinessKind)
        : documentStatusLabel(detail.status),
    },
    detail.activeRevisionNo
      ? {
          key: 'revision',
          label: documentMetadataLabel('activeRevision'),
          value: `#${String(detail.activeRevisionNo)}`,
        }
      : null,
    detail.extractedStats.chunkCount !== null
      ? {
          key: 'chunks',
          label: documentMetadataLabel('chunkCount'),
          value: String(detail.extractedStats.chunkCount),
        }
      : null,
    detail.totalEstimatedCost !== null
      ? {
          key: 'totalCost',
          label: documentMetadataLabel('totalCost'),
          value: formatCost(detail.totalEstimatedCost, detail.currency),
        }
      : null,
    detail.providerCallCount > 0
      ? {
          key: 'providerCalls',
          label: documentMetadataLabel('providerCalls'),
          value: String(detail.providerCallCount),
        }
      : null,
  ].filter((item): item is { key: string; label: string; value: string } => item !== null)
})

const webSourceRows = computed(() => {
  const detail = props.detail
  if (detail?.contentSourceKind !== 'web_page') {
    return []
  }
  return [
    detail.sourceUri
      ? {
          key: 'sourceUri',
          label: t('documents.details.webSourceUri'),
          value: detail.sourceUri,
          href: detail.sourceUri,
        }
      : null,
    {
      key: 'contentSourceKind',
      label: t('documents.details.webSourceKind'),
      value: detail.contentSourceKind,
      href: null,
    },
    detail.webPageProvenance?.canonicalUrl
      ? {
          key: 'canonicalUrl',
          label: t('documents.details.webCanonicalUrl'),
          value: detail.webPageProvenance.canonicalUrl,
          href: detail.webPageProvenance.canonicalUrl,
        }
      : null,
    detail.webPageProvenance?.runId
      ? {
          key: 'runId',
          label: t('documents.details.webRunId'),
          value: detail.webPageProvenance.runId,
          href: null,
        }
      : null,
    detail.webPageProvenance?.candidateId
      ? {
          key: 'candidateId',
          label: t('documents.details.webCandidateId'),
          value: detail.webPageProvenance.candidateId,
          href: null,
        }
      : null,
    props.webRunCandidate
      ? {
          key: 'candidateState',
          label: t('documents.details.webCandidateState'),
          value: t(`documents.webRuns.candidateStates.${props.webRunCandidate.candidateState}`),
          href: null,
        }
      : null,
    props.webRunCandidate?.classificationReason
      ? {
          key: 'candidateReason',
          label: t('documents.details.webCandidateReason'),
          value: t(`documents.webRuns.reasons.${props.webRunCandidate.classificationReason}`),
          href: null,
        }
      : null,
  ].filter(
    (item): item is { key: string; label: string; value: string; href: string | null } =>
      item !== null,
  )
})

const provenanceRunId = computed(() => props.detail?.webPageProvenance?.runId ?? null)
</script>

<template>
  <aside v-if="props.open" class="rr-document-inspector">
    <header class="rr-document-inspector__header">
      <div class="rr-document-inspector__header-main">
        <div class="rr-document-inspector__copy">
          <span class="rr-document-inspector__eyebrow">{{ $t('documents.headers.document') }}</span>
          <h2 v-if="props.detail">{{ props.detail.fileName }}</h2>
          <h2 v-else>{{ $t('documents.details.title') }}</h2>
          <p v-if="props.detail">{{ metaLine }}</p>
        </div>

        <button
          type="button"
          class="rr-button rr-button--ghost rr-button--tiny"
          @click="emit('close')"
        >
          {{ $t('dialogs.close') }}
        </button>
      </div>

      <div v-if="props.detail" class="rr-document-inspector__summary-strip">
        <div class="rr-document-inspector__summary-strip-main">
          <StatusPill :tone="statusTone" :label="statusLabel" />
        </div>
        <div v-if="hasQuickExploreActions" class="rr-document-inspector__summary-strip-actions">
          <button
            v-if="props.detail.graphNodeId"
            class="rr-button rr-button--ghost rr-button--tiny"
            type="button"
            @click="emit('openInGraph', props.detail.graphNodeId)"
          >
            {{ $t('documents.details.openInGraph') }}
          </button>
          <button
            v-if="previewText && props.detail.canDownloadText"
            class="rr-button rr-button--ghost rr-button--tiny"
            type="button"
            :disabled="props.downloadingId === props.detail.id"
            @click="emit('downloadText', props.detail.id)"
          >
            {{
              props.downloadingId === props.detail.id ? '…' : $t('documents.details.downloadText')
            }}
          </button>
        </div>
      </div>
    </header>

    <div v-if="props.loading" class="rr-document-inspector__empty">
      {{ $t('documents.loadingDetail') }}
    </div>

    <div v-else-if="props.error" class="rr-document-inspector__empty">
      {{ props.error }}
    </div>

    <template v-else-if="props.detail">
      <p v-if="summaryLine" class="rr-document-inspector__lead">
        {{ summaryLine }}
      </p>

      <section v-if="failureSummaryText" class="rr-document-inspector__failure-block">
        <q-banner
          dense
          rounded
          class="rr-document-inspector__failure-banner"
          :class="{ 'is-muted': failureIsSupersededRun }"
        >
          <template #avatar>
            <span class="rr-document-inspector__failure-dot" aria-hidden="true" />
          </template>

          <div class="rr-document-inspector__failure-copy">
            <strong>{{ failureTitle }}</strong>
            <p>{{ failureSummaryText }}</p>
          </div>

          <template #action>
            <button
              v-if="failureDetailsText"
              type="button"
              class="rr-document-inspector__link-button"
              @click="failureExpanded = !failureExpanded"
            >
              {{
                failureExpanded
                  ? $t('documents.details.showLess')
                  : $t('documents.details.showMore')
              }}
            </button>
          </template>
        </q-banner>

        <div
          v-if="failureDetailsText && failureExpanded"
          class="rr-document-inspector__failure-details"
        >
          <div class="rr-document-inspector__failure-raw">
            {{ failureDetailsText }}
          </div>
        </div>
      </section>

      <section class="rr-document-inspector__section">
        <div class="rr-document-inspector__section-head">
          <strong>{{ $t('documents.details.preparationStatus') }}</strong>
        </div>
        <DocumentPreparationStatusCard
          v-if="preparationSummary"
          :readiness-kind="preparationSummary.readinessKind"
          :graph-coverage-kind="preparationSummary.graphCoverageKind"
          :typed-fact-coverage="preparationSummary.typedFactCoverage"
          :last-processing-stage="preparationSummary.lastProcessingStage"
          :preparation-state="preparationSummary.preparationState"
          :prepared-segment-count="preparationSummary.preparedSegmentCount"
          :technical-fact-count="preparationSummary.technicalFactCount"
          :source-format="preparationSummary.sourceFormat"
          :normalization-profile="preparationSummary.normalizationProfile"
          :updated-at="preparationSummary.updatedAt"
        />
        <p v-else class="rr-document-inspector__microcopy">
          {{ $t('documents.details.noPreparationSummary') }}
        </p>
      </section>

      <section v-if="hasPreparedSegments" class="rr-document-inspector__section">
        <details class="rr-document-inspector__disclosure">
          <summary class="rr-document-inspector__disclosure-summary">
            <div class="rr-document-inspector__disclosure-copy">
              <strong>{{ $t('documents.details.preparedSegments') }}</strong>
              <span
                >{{ $t('documents.details.preparedSegmentsCount') }} ·
                {{ preparedSegmentsTotal }}</span
              >
            </div>
            <span class="rr-document-inspector__disclosure-chevron" aria-hidden="true">⌄</span>
          </summary>
          <DocumentPreparedSegmentsList
            :items="preparedSegments"
            :total-count="preparedSegmentsTotal"
          />
        </details>
      </section>

      <section v-if="hasTechnicalFacts" class="rr-document-inspector__section">
        <details class="rr-document-inspector__disclosure">
          <summary class="rr-document-inspector__disclosure-summary">
            <div class="rr-document-inspector__disclosure-copy">
              <strong>{{ $t('documents.details.technicalFacts') }}</strong>
              <span
                >{{ $t('documents.details.technicalFactsCount') }} · {{ technicalFactsTotal }}</span
              >
            </div>
            <span class="rr-document-inspector__disclosure-chevron" aria-hidden="true">⌄</span>
          </summary>
          <DocumentTechnicalFactsList :items="technicalFacts" :total-count="technicalFactsTotal" />
        </details>
      </section>

      <section class="rr-document-inspector__section">
        <div class="rr-document-inspector__section-head">
          <strong>{{ $t('documents.details.keyInfo') }}</strong>
        </div>
        <div class="rr-document-inspector__fact-grid">
          <article
            v-for="item in overviewRows"
            :key="item.key"
            class="rr-document-inspector__fact-card"
          >
            <span class="rr-document-inspector__fact-label">{{ item.label }}</span>
            <strong class="rr-document-inspector__fact-value">{{ item.value }}</strong>
          </article>
        </div>
        <div v-if="webSourceRows.length" class="rr-document-inspector__activity">
          <div
            v-for="item in webSourceRows"
            :key="item.key"
            class="rr-document-inspector__activity-row"
          >
            <span class="rr-document-inspector__activity-label">{{ item.label }}</span>
            <a
              v-if="item.href"
              class="rr-document-inspector__activity-link"
              :href="item.href"
              target="_blank"
              rel="noreferrer"
            >
              {{ item.value }}
            </a>
            <strong v-else class="rr-document-inspector__activity-value">
              {{ item.value }}
            </strong>
          </div>
          <div v-if="provenanceRunId" class="rr-document-inspector__activity-row">
            <span class="rr-document-inspector__activity-label">{{
              $t('documents.details.webRunActions')
            }}</span>
            <button
              type="button"
              class="rr-button rr-button--ghost rr-button--tiny"
              @click="emit('openWebRun', provenanceRunId)"
            >
              {{ $t('documents.details.openWebRun') }}
            </button>
          </div>
        </div>
        <div v-if="mutationSummary || mutationWarningLabel" class="rr-document-inspector__activity">
          <div v-if="mutationSummary" class="rr-document-inspector__activity-row">
            <span class="rr-document-inspector__activity-label">{{
              $t('documents.details.latestChange')
            }}</span>
            <strong class="rr-document-inspector__activity-value">{{ mutationSummary }}</strong>
          </div>
          <p v-if="mutationWarningLabel" class="rr-document-inspector__microcopy">
            {{ mutationWarningLabel }}
          </p>
        </div>
      </section>

      <section class="rr-document-inspector__section">
        <div class="rr-document-inspector__section-head">
          <strong>{{ $t('documents.details.readableText') }}</strong>
          <button
            v-if="previewText && previewIsLong"
            type="button"
            class="rr-document-inspector__link-button"
            @click="previewExpanded = !previewExpanded"
          >
            {{
              previewExpanded ? $t('documents.details.showLess') : $t('documents.details.showMore')
            }}
          </button>
        </div>
        <p v-if="previewText" class="rr-document-inspector__preview">
          {{ previewVisibleText }}
        </p>
        <p v-else class="rr-document-inspector__microcopy">
          {{ $t('documents.details.notReadableYet') }}
        </p>
        <p
          v-if="previewText && previewIsLong && !previewExpanded"
          class="rr-document-inspector__microcopy"
        >
          {{ $t('documents.details.previewTruncated') }}
        </p>
      </section>

      <section class="rr-document-inspector__section rr-document-inspector__actions">
        <div class="rr-document-inspector__action-group">
          <span class="rr-document-inspector__action-label">{{
            $t('documents.actions.groups.update')
          }}</span>
          <div class="rr-document-inspector__action-row">
            <button
              v-if="props.detail.canAppend"
              class="rr-button rr-button--secondary rr-button--tiny"
              type="button"
              @click="emit('append', props.detail.id)"
            >
              {{ $t('documents.actions.append') }}
            </button>
            <button
              v-if="props.detail.canReplace"
              class="rr-button rr-button--secondary rr-button--tiny"
              type="button"
              @click="emit('replace', props.detail.id)"
            >
              {{ $t('documents.actions.replace') }}
            </button>
          </div>
        </div>

        <div class="rr-document-inspector__action-group">
          <span class="rr-document-inspector__action-label">{{
            $t('documents.actions.groups.recovery')
          }}</span>
          <div class="rr-document-inspector__action-row">
            <button
              v-if="props.detail.canRetry"
              class="rr-button rr-button--ghost rr-button--tiny"
              type="button"
              @click="emit('retry', props.detail.id)"
            >
              {{ $t('documents.actions.retry') }}
            </button>
            <button
              v-if="props.detail.canRemove"
              class="rr-button rr-button--ghost rr-button--tiny is-danger"
              type="button"
              @click="emit('remove', props.detail.id)"
            >
              {{ $t('documents.actions.remove') }}
            </button>
          </div>
        </div>
      </section>
    </template>

    <div v-else class="rr-document-inspector__empty">
      {{ $t('documents.details.empty') }}
    </div>
  </aside>
</template>

<style scoped lang="scss">
.rr-document-inspector {
  display: grid;
  gap: 0.82rem;
  padding: 0.92rem;
  border: 1px solid rgba(15, 23, 42, 0.07);
  border-radius: 1.1rem;
  background: rgba(255, 255, 255, 0.96);
  box-shadow: 0 14px 28px rgba(15, 23, 42, 0.045);
}

.rr-document-inspector__header,
.rr-document-inspector__section,
.rr-document-inspector__actions {
  display: grid;
  gap: 0.58rem;
}

.rr-document-inspector__header-main {
  display: flex;
  align-items: start;
  justify-content: space-between;
  gap: 0.75rem;
}

.rr-document-inspector__copy {
  display: grid;
  gap: 0.35rem;
}

.rr-document-inspector__copy h2 {
  margin: 0;
  font-size: 1.22rem;
  line-height: 1.08;
  letter-spacing: -0.035em;
  word-break: break-word;
  overflow-wrap: anywhere;
}

.rr-document-inspector__copy p,
.rr-document-inspector__microcopy,
.rr-document-inspector__preview,
.rr-document-inspector__lead,
.rr-document-inspector__empty {
  margin: 0;
}

.rr-document-inspector__eyebrow {
  color: rgba(15, 23, 42, 0.5);
  font-size: 0.78rem;
  font-weight: 700;
  letter-spacing: 0.08em;
  text-transform: uppercase;
}

.rr-document-inspector__summary-strip {
  display: flex;
  flex-wrap: wrap;
  align-items: flex-start;
  justify-content: space-between;
  gap: 0.6rem 0.75rem;
}

.rr-document-inspector__summary-strip-main,
.rr-document-inspector__summary-strip-actions {
  display: flex;
  flex-wrap: wrap;
  gap: 0.45rem;
}

.rr-document-inspector__lead {
  padding: 0.64rem 0.72rem;
  border-radius: 0.82rem;
  background: rgba(247, 249, 252, 0.92);
  color: rgba(15, 23, 42, 0.72);
  font-size: 0.86rem;
  line-height: 1.5;
}

.rr-document-inspector__failure-block {
  display: grid;
  gap: 0.42rem;
}

.rr-document-inspector__failure-banner {
  border: 1px solid rgba(248, 113, 113, 0.24);
  border-radius: 0.9rem;
  background: linear-gradient(180deg, rgba(255, 244, 244, 0.96), rgba(255, 250, 250, 0.98));
}

.rr-document-inspector__failure-banner.is-muted {
  border-color: rgba(245, 158, 11, 0.2);
  background: linear-gradient(180deg, rgba(255, 251, 235, 0.98), rgba(255, 253, 244, 0.98));
}

.rr-document-inspector__failure-dot {
  display: inline-flex;
  width: 0.52rem;
  height: 0.52rem;
  border-radius: 999px;
  background: rgba(220, 38, 38, 0.96);
  box-shadow: 0 0 0 0.25rem rgba(254, 226, 226, 0.92);
}

.rr-document-inspector__failure-banner.is-muted .rr-document-inspector__failure-dot {
  background: rgba(217, 119, 6, 0.96);
  box-shadow: 0 0 0 0.25rem rgba(254, 243, 199, 0.92);
}

.rr-document-inspector__failure-copy {
  display: grid;
  gap: 0.22rem;
}

.rr-document-inspector__failure-copy strong,
.rr-document-inspector__failure-copy p {
  margin: 0;
}

.rr-document-inspector__failure-copy strong {
  color: rgba(185, 28, 28, 0.92);
  font-size: 0.84rem;
  letter-spacing: 0.06em;
  text-transform: uppercase;
}

.rr-document-inspector__failure-banner.is-muted .rr-document-inspector__failure-copy strong {
  color: rgba(146, 64, 14, 0.94);
}

.rr-document-inspector__failure-copy p {
  color: rgba(127, 29, 29, 0.9);
  font-size: 0.84rem;
  line-height: 1.45;
}

.rr-document-inspector__failure-banner.is-muted .rr-document-inspector__failure-copy p {
  color: rgba(120, 53, 15, 0.88);
}

.rr-document-inspector__failure-details {
  border-color: rgba(248, 113, 113, 0.18);
  background: rgba(255, 250, 250, 0.96);
}

.rr-document-inspector__failure-raw {
  padding: 0.72rem 0.74rem;
  border-radius: 0.82rem;
  background: rgba(255, 244, 244, 0.92);
  color: rgba(127, 29, 29, 0.88);
  font-size: 0.82rem;
  line-height: 1.52;
  white-space: pre-wrap;
  word-break: break-word;
}

.rr-document-inspector__section {
  padding-top: 0.74rem;
  border-top: 1px solid rgba(15, 23, 42, 0.07);
}

.rr-document-inspector__section-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 0.75rem;
}

.rr-document-inspector__preview {
  max-height: 18rem;
  overflow: auto;
  padding: 0.72rem 0.76rem;
  border-radius: 0.82rem;
  background: rgba(247, 249, 252, 0.92);
  color: rgba(15, 23, 42, 0.82);
  font-size: 0.89rem;
  line-height: 1.58;
  white-space: pre-wrap;
}

.rr-document-inspector__microcopy {
  color: rgba(15, 23, 42, 0.56);
  font-size: 0.9rem;
  line-height: 1.45;
}

.rr-document-inspector__link-button {
  border: 0;
  padding: 0;
  background: transparent;
  color: rgba(59, 130, 246, 0.9);
  font: inherit;
  font-size: 0.84rem;
  font-weight: 600;
  cursor: pointer;
}

.rr-document-inspector__link-button:hover {
  color: rgba(37, 99, 235, 0.96);
}

.rr-document-inspector__fact-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 0.58rem;
}

.rr-document-inspector__fact-card {
  display: grid;
  gap: 0.28rem;
  padding: 0.62rem 0.68rem;
  border: 1px solid rgba(226, 232, 240, 0.86);
  border-radius: 0.82rem;
  background: rgba(255, 255, 255, 0.92);
}

.rr-document-inspector__fact-label {
  font-size: 0.72rem;
  font-weight: 700;
  letter-spacing: 0.06em;
  text-transform: uppercase;
  color: rgba(15, 23, 42, 0.46);
}

.rr-document-inspector__fact-value {
  color: rgba(15, 23, 42, 0.9);
  font-size: 0.92rem;
  line-height: 1.4;
  font-variant-numeric: tabular-nums;
}

.rr-document-inspector__activity {
  display: grid;
  gap: 0.48rem;
  padding: 0.66rem 0.74rem;
  border-radius: 0.82rem;
  background: rgba(245, 247, 255, 0.92);
}

.rr-document-inspector__disclosure,
.rr-document-inspector__failure-details {
  border: 1px solid rgba(226, 232, 240, 0.86);
  border-radius: 0.9rem;
  background: rgba(255, 255, 255, 0.94);
  overflow: hidden;
}

.rr-document-inspector__disclosure-summary {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 0.8rem;
  padding: 0.72rem 0.78rem;
  cursor: pointer;
  list-style: none;
}

.rr-document-inspector__disclosure-summary::-webkit-details-marker {
  display: none;
}

.rr-document-inspector__disclosure-copy {
  display: grid;
  gap: 0.12rem;
  min-width: 0;
}

.rr-document-inspector__disclosure-copy strong {
  color: rgba(15, 23, 42, 0.9);
  font-size: 0.9rem;
  font-weight: 650;
}

.rr-document-inspector__disclosure-copy span {
  color: rgba(71, 85, 105, 0.82);
  font-size: 0.74rem;
  font-weight: 500;
}

.rr-document-inspector__disclosure-chevron {
  color: rgba(100, 116, 139, 0.86);
  font-size: 1rem;
  line-height: 1;
  transition: transform 140ms ease;
}

.rr-document-inspector__disclosure[open] .rr-document-inspector__disclosure-chevron {
  transform: rotate(180deg);
}

.rr-document-inspector__disclosure > :not(summary) {
  padding: 0 0.76rem 0.72rem;
}

.rr-document-inspector__activity-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 0.75rem;
  flex-wrap: wrap;
}

.rr-document-inspector__activity-label {
  font-size: 0.78rem;
  font-weight: 700;
  letter-spacing: 0.08em;
  text-transform: uppercase;
  color: rgba(15, 23, 42, 0.48);
}

.rr-document-inspector__activity-value {
  color: rgba(15, 23, 42, 0.86);
  font-size: 0.88rem;
  font-weight: 700;
  line-height: 1.4;
}

.rr-document-inspector__activity-link {
  color: rgba(29, 78, 216, 0.94);
  font-size: 0.88rem;
  font-weight: 700;
  line-height: 1.4;
  text-decoration: underline;
  text-decoration-color: rgba(59, 130, 246, 0.35);
  text-underline-offset: 0.18em;
  word-break: break-word;
}

.rr-document-inspector__activity-link:hover {
  color: rgba(30, 64, 175, 0.96);
  text-decoration-color: rgba(37, 99, 235, 0.6);
}

.rr-document-inspector__actions {
  gap: 0.74rem;
  padding-top: 0.74rem;
  border-top: 1px solid rgba(15, 23, 42, 0.07);
}

.rr-document-inspector__action-group {
  display: grid;
  gap: 0.48rem;
  padding: 0.72rem 0.82rem;
  border: 1px solid rgba(226, 232, 240, 0.82);
  border-radius: 0.95rem;
  background: rgba(255, 255, 255, 0.92);
}

.rr-document-inspector__action-label {
  font-size: 0.78rem;
  font-weight: 700;
  letter-spacing: 0.08em;
  text-transform: uppercase;
  color: rgba(15, 23, 42, 0.48);
}

.rr-document-inspector__action-row {
  display: flex;
  flex-wrap: wrap;
  gap: 0.5rem;
}

.rr-document-inspector__empty {
  display: grid;
  place-items: center;
  min-height: 10rem;
  color: rgba(15, 23, 42, 0.56);
}

@media (min-width: 1280px) {
  .rr-document-inspector__fact-grid {
    grid-template-columns: repeat(3, minmax(0, 1fr));
  }

  .rr-document-inspector__actions {
    grid-template-columns: repeat(2, minmax(0, 1fr));
    align-items: start;
  }

  .rr-document-inspector__action-group {
    height: 100%;
  }
}

@media (min-width: 1025px) {
  .rr-document-inspector {
    position: sticky;
    top: 1rem;
    max-height: calc(100vh - 7rem);
    overflow: auto;
  }
}

@media (min-width: 1800px) {
  .rr-document-inspector {
    gap: 0.9rem;
    padding: 1rem;
  }

  .rr-document-inspector__header,
  .rr-document-inspector__section,
  .rr-document-inspector__actions {
    gap: 0.6rem;
  }

  .rr-document-inspector__summary-strip {
    align-items: center;
  }

  .rr-document-inspector__fact-grid {
    grid-template-columns: repeat(3, minmax(0, 1fr));
  }
}

@media (max-width: 820px) {
  .rr-document-inspector {
    max-height: none;
    min-height: 100%;
    overflow: auto;
  }

  .rr-document-inspector__header {
    position: sticky;
    top: 0;
    z-index: 2;
    margin: -1rem -1rem 0;
    padding: 0.9rem 1rem 0.72rem;
    background: rgba(255, 255, 255, 0.96);
    border-bottom: 1px solid rgba(226, 232, 240, 0.88);
    backdrop-filter: blur(12px);
  }

  .rr-document-inspector__eyebrow {
    display: none;
  }

  .rr-document-inspector__copy {
    gap: 0.22rem;
  }

  .rr-document-inspector__copy h2 {
    font-size: 1.02rem;
    line-height: 1.14;
  }

  .rr-document-inspector__copy p {
    font-size: 0.8rem;
    color: rgba(15, 23, 42, 0.62);
  }

  .rr-document-inspector__summary-strip {
    gap: 0.45rem;
  }

  .rr-document-inspector__summary-strip-actions {
    gap: 0.4rem;
  }
}

@media (max-width: 720px) {
  .rr-document-inspector {
    gap: 0.85rem;
    padding: 1rem;
    border-radius: 1rem;
  }

  .rr-document-inspector__header-main {
    align-items: flex-start;
  }

  .rr-document-inspector__summary-strip {
    align-items: stretch;
    flex-direction: column;
  }

  .rr-document-inspector__header {
    margin: -1rem -1rem 0;
    padding: 0.82rem 1rem 0.68rem;
  }
}

@media (max-width: 640px) {
  .rr-document-inspector__fact-grid {
    grid-template-columns: 1fr;
  }

  .rr-document-inspector__activity-row {
    align-items: flex-start;
  }
}
</style>
