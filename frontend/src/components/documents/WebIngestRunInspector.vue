<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import StatusBadge from 'src/components/design-system/StatusBadge.vue'
import WebIngestRunPagesTable from './WebIngestRunPagesTable.vue'
import type { WebIngestRun, WebRunState, WebDiscoveredPage } from 'src/models/ui/documents'

const props = defineProps<{
  open: boolean
  detail: WebIngestRun | null
  pages: WebDiscoveredPage[]
  loading: boolean
  error: string | null
  actionLoading?: boolean
}>()

const emit = defineEmits<{
  close: []
  cancel: [runId: string]
  openDocument: [documentId: string]
}>()

const { t, te } = useI18n()

function stateLabel(state: WebRunState): string {
  const key = `documents.webRuns.states.${state}`
  return te(key) ? t(key) : state
}

function stateTone(
  state: WebRunState,
): 'queued' | 'processing' | 'ready' | 'partial' | 'failed' | 'disabled' | 'info' {
  switch (state) {
    case 'accepted':
      return 'queued'
    case 'discovering':
    case 'processing':
      return 'processing'
    case 'completed':
      return 'ready'
    case 'completed_partial':
      return 'partial'
    case 'failed':
      return 'failed'
    case 'canceled':
      return 'disabled'
    default:
      return 'info'
  }
}

function modeLabel(value: string): string {
  return t(`documents.webRuns.modes.${value}`)
}

function boundaryLabel(value: string): string {
  return t(`documents.webRuns.boundaries.${value}`)
}

function failureCodeLabel(value: string | null): string | null {
  if (!value) {
    return null
  }
  const key = `documents.webRuns.failureCodes.${value}`
  return te(key) ? t(key) : value
}

function formatDateTime(value: string | null): string {
  if (!value) {
    return '—'
  }
  return new Intl.DateTimeFormat(undefined, {
    dateStyle: 'medium',
    timeStyle: 'short',
  }).format(new Date(value))
}

const canCancel = computed(() =>
  Boolean(
    props.detail &&
    !props.detail.cancelRequestedAt &&
    ['accepted', 'discovering', 'processing'].includes(props.detail.runState),
  ),
)

const overviewRows = computed(() => {
  const detail = props.detail
  if (!detail) {
    return []
  }
  return [
    {
      key: 'mode',
      label: t('documents.webRuns.fields.mode'),
      value: modeLabel(detail.mode),
    },
    {
      key: 'boundary',
      label: t('documents.webRuns.fields.boundary'),
      value: boundaryLabel(detail.boundaryPolicy),
    },
    {
      key: 'depth',
      label: t('documents.webRuns.fields.maxDepth'),
      value: String(detail.maxDepth),
    },
    {
      key: 'pages',
      label: t('documents.webRuns.fields.maxPages'),
      value: String(detail.maxPages),
    },
    {
      key: 'requested',
      label: t('documents.webRuns.fields.requestedAt'),
      value: formatDateTime(detail.requestedAt),
    },
    detail.cancelRequestedAt
      ? {
          key: 'cancelRequested',
          label: t('documents.webRuns.fields.cancelRequestedAt'),
          value: formatDateTime(detail.cancelRequestedAt),
        }
      : null,
    {
      key: 'completed',
      label: t('documents.webRuns.fields.completedAt'),
      value: formatDateTime(detail.completedAt),
    },
  ].filter((row): row is { key: string; label: string; value: string } => Boolean(row))
})

const countRows = computed(() => {
  const detail = props.detail
  if (!detail) {
    return []
  }
  return [
    {
      key: 'discovered',
      label: t('documents.webRuns.counts.discovered'),
      value: detail.counts.discovered,
    },
    {
      key: 'eligible',
      label: t('documents.webRuns.counts.eligible'),
      value: detail.counts.eligible,
    },
    {
      key: 'processed',
      label: t('documents.webRuns.counts.processed'),
      value: detail.counts.processed,
    },
    { key: 'queued', label: t('documents.webRuns.counts.queued'), value: detail.counts.queued },
    {
      key: 'processing',
      label: t('documents.webRuns.counts.processing'),
      value: detail.counts.processing,
    },
    {
      key: 'duplicates',
      label: t('documents.webRuns.counts.duplicates'),
      value: detail.counts.duplicates,
    },
    {
      key: 'excluded',
      label: t('documents.webRuns.counts.excluded'),
      value: detail.counts.excluded,
    },
    { key: 'blocked', label: t('documents.webRuns.counts.blocked'), value: detail.counts.blocked },
    { key: 'failed', label: t('documents.webRuns.counts.failed'), value: detail.counts.failed },
    {
      key: 'canceled',
      label: t('documents.webRuns.counts.canceled'),
      value: detail.counts.canceled,
    },
  ]
})
</script>

<template>
  <aside v-if="props.open" class="rr-web-run-inspector">
    <header class="rr-web-run-inspector__header">
      <div class="rr-web-run-inspector__copy">
        <span class="rr-web-run-inspector__eyebrow">{{
          $t('documents.webRuns.inspector.eyebrow')
        }}</span>
        <h2>{{ props.detail?.seedUrl ?? $t('documents.webRuns.inspector.title') }}</h2>
        <p v-if="props.detail">{{ $t('documents.webRuns.inspector.subtitle') }}</p>
      </div>

      <div class="rr-web-run-inspector__actions">
        <button
          v-if="props.detail && canCancel"
          type="button"
          class="rr-button rr-button--secondary rr-button--tiny"
          :disabled="props.actionLoading"
          @click="emit('cancel', props.detail.runId)"
        >
          {{ $t('documents.webRuns.actions.cancel') }}
        </button>
        <button
          type="button"
          class="rr-button rr-button--ghost rr-button--tiny"
          @click="emit('close')"
        >
          {{ $t('dialogs.close') }}
        </button>
      </div>
    </header>

    <div v-if="props.loading" class="rr-web-run-inspector__empty">
      {{ $t('documents.webRuns.inspector.loading') }}
    </div>

    <div v-else-if="props.error" class="rr-web-run-inspector__empty">
      {{ props.error }}
    </div>

    <template v-else-if="props.detail">
      <section class="rr-web-run-inspector__summary">
        <div class="rr-web-run-inspector__summary-main">
          <StatusBadge
            :kind="stateTone(props.detail.runState)"
            :label="stateLabel(props.detail.runState)"
          />
          <code>{{ props.detail.seedUrl }}</code>
        </div>
        <p v-if="props.detail.cancelRequestedAt" class="rr-web-run-inspector__failure">
          {{
            $t('documents.webRuns.activity.cancelRequestedAt', {
              value: formatDateTime(props.detail.cancelRequestedAt),
            })
          }}
        </p>
        <p v-if="props.detail.failureCode" class="rr-web-run-inspector__failure">
          {{ $t('documents.webRuns.fields.failureCode') }}:
          {{ failureCodeLabel(props.detail.failureCode) }}
        </p>
      </section>

      <section class="rr-web-run-inspector__section">
        <div class="rr-web-run-inspector__kv">
          <div v-for="row in overviewRows" :key="row.key" class="rr-web-run-inspector__kv-row">
            <span>{{ row.label }}</span>
            <strong>{{ row.value }}</strong>
          </div>
        </div>
      </section>

      <section class="rr-web-run-inspector__section">
        <div class="rr-web-run-inspector__count-grid">
          <article v-for="row in countRows" :key="row.key" class="rr-web-run-inspector__count-card">
            <strong>{{ row.value }}</strong>
            <span>{{ row.label }}</span>
          </article>
        </div>
      </section>

      <WebIngestRunPagesTable :pages="props.pages" @open-document="emit('openDocument', $event)" />
    </template>
  </aside>
</template>

<style scoped lang="scss">
.rr-web-run-inspector {
  display: grid;
  gap: 12px;
  min-height: 100%;
  padding: 14px;
  border: 1px solid rgba(226, 232, 240, 0.92);
  border-radius: 20px;
  background:
    radial-gradient(circle at top right, rgba(14, 165, 233, 0.06), transparent 28%),
    rgba(255, 255, 255, 0.98);
  box-shadow: 0 16px 32px rgba(15, 23, 42, 0.05);
}

.rr-web-run-inspector__header {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 12px;
}

.rr-web-run-inspector__copy {
  display: grid;
  gap: 4px;
  min-width: 0;
}

.rr-web-run-inspector__copy h2 {
  margin: 0;
  font-size: 0.98rem;
  line-height: 1.32;
  word-break: break-word;
}

.rr-web-run-inspector__copy p,
.rr-web-run-inspector__failure,
.rr-web-run-inspector__empty {
  margin: 0;
  color: var(--rr-text-secondary);
  font-size: 0.76rem;
  line-height: 1.42;
}

.rr-web-run-inspector__eyebrow {
  color: rgba(14, 116, 144, 0.92);
  font-size: 0.68rem;
  font-weight: 700;
  letter-spacing: 0.08em;
  text-transform: uppercase;
}

.rr-web-run-inspector__actions {
  display: inline-flex;
  flex-wrap: wrap;
  gap: 6px;
}

.rr-web-run-inspector__summary {
  display: grid;
  gap: 8px;
  padding: 10px 12px;
  border: 1px solid rgba(226, 232, 240, 0.88);
  border-radius: 14px;
  background: rgba(248, 250, 252, 0.88);
}

.rr-web-run-inspector__summary-main {
  display: grid;
  gap: 8px;
}

.rr-web-run-inspector__summary-main code {
  display: block;
  min-width: 0;
  padding: 8px 10px;
  border-radius: 10px;
  background: rgba(255, 255, 255, 0.92);
  color: rgba(15, 23, 42, 0.92);
  font-size: 0.73rem;
  word-break: break-all;
}

.rr-web-run-inspector__failure {
  padding: 8px 10px;
  border-radius: 10px;
  background: rgba(255, 255, 255, 0.82);
}

.rr-web-run-inspector__section {
  padding-top: 4px;
  border-top: 1px solid rgba(226, 232, 240, 0.88);
}

.rr-web-run-inspector__kv {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 8px;
}

.rr-web-run-inspector__kv-row,
.rr-web-run-inspector__count-card {
  display: grid;
  gap: 3px;
}

.rr-web-run-inspector__kv-row span,
.rr-web-run-inspector__count-card span {
  color: var(--rr-text-secondary);
  font-size: 0.72rem;
}

.rr-web-run-inspector__kv-row strong,
.rr-web-run-inspector__count-card strong {
  color: var(--rr-text-primary);
  font-size: 0.88rem;
}

.rr-web-run-inspector__count-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(92px, 1fr));
  gap: 6px;
}

.rr-web-run-inspector__count-card {
  padding: 8px 9px;
  border: 1px solid rgba(226, 232, 240, 0.86);
  border-radius: 12px;
  background: rgba(248, 250, 252, 0.94);
}

@media (max-width: 1180px) {
  .rr-web-run-inspector__header {
    flex-direction: column;
  }

  .rr-web-run-inspector__kv,
  .rr-web-run-inspector__count-grid {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }
}

@media (max-width: 640px) {
  .rr-web-run-inspector {
    padding: 12px;
    border-radius: 18px;
  }

  .rr-web-run-inspector__summary-main {
    gap: 6px;
  }

  .rr-web-run-inspector__kv {
    grid-template-columns: 1fr;
  }
}
</style>
