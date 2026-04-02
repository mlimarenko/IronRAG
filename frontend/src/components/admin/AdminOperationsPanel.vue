<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { useDisplayFormatters } from 'src/composables/useDisplayFormatters'
import type { AdminAuditEvent, AdminOpsLibrarySnapshot } from 'src/models/ui/admin'

const props = defineProps<{
  snapshot: AdminOpsLibrarySnapshot | null
  events: AdminAuditEvent[]
}>()

const { t } = useI18n()
const {
  auditActionLabel,
  auditSubjectLabel,
  enumLabel,
  formatDateTime,
  humanizeToken,
  shortIdentifier,
  statusBadgeLabel,
} = useDisplayFormatters()

const metrics = computed(() => {
  if (!props.snapshot) {
    return []
  }
  return [
    {
      key: 'queue',
      label: t('admin.ops.metrics.queue'),
      value: props.snapshot.state.queueDepth,
    },
    {
      key: 'running',
      label: t('admin.ops.metrics.running'),
      value: props.snapshot.state.runningAttempts,
    },
    {
      key: 'readable',
      label: t('admin.ops.metrics.readable'),
      value: props.snapshot.state.readableDocumentCount,
    },
    {
      key: 'failed',
      label: t('admin.ops.metrics.failed'),
      value: props.snapshot.state.failedDocumentCount,
    },
  ]
})

const activeWarnings = computed(() =>
  (props.snapshot?.warnings ?? []).filter((warning) => warning.resolvedAt === null).slice(0, 6),
)
const hasWarnings = computed(() => activeWarnings.value.length > 0)

const visibleEvents = computed(() => props.events.slice(0, 6))

const headline = computed(() => {
  if (!props.snapshot) {
    return {
      title: t('admin.ops.unavailable'),
      detail: t('admin.audit.empty'),
      toneClass: 'is-muted',
    }
  }

  if (activeWarnings.value.length > 0 || props.snapshot.state.failedDocumentCount > 0) {
    return {
      title: `${t('admin.ops.warningsTitle')}: ${activeWarnings.value.length}`,
      detail: `${t('admin.ops.metrics.failed')}: ${props.snapshot.state.failedDocumentCount} · ${t('admin.ops.metrics.running')}: ${props.snapshot.state.runningAttempts}`,
      toneClass: 'is-danger',
    }
  }

  if (props.snapshot.state.degradedState !== 'healthy' || props.snapshot.state.queueDepth > 0) {
    return {
      title: `${t('admin.ops.healthTitle')}: ${statusBadgeLabel(props.snapshot.state.degradedState)}`,
      detail: `${t('admin.ops.metrics.queue')}: ${props.snapshot.state.queueDepth} · ${t('admin.ops.metrics.running')}: ${props.snapshot.state.runningAttempts}`,
      toneClass: 'is-warning',
    }
  }

  return {
    title: `${t('admin.ops.healthTitle')}: ${statusBadgeLabel(props.snapshot.state.degradedState)}`,
    detail: `${t('admin.ops.metrics.readable')}: ${props.snapshot.state.readableDocumentCount}`,
    toneClass: 'is-success',
  }
})

function severityClass(value: string): string {
  switch (value) {
    case 'critical':
    case 'error':
      return 'is-danger'
    case 'warning':
      return 'is-warning'
    default:
      return 'is-muted'
  }
}

function warningLabel(value: string): string {
  return enumLabel('admin.ops.warningKinds', value, humanizeToken(value))
}

function subjectSummary(event: AdminAuditEvent): string {
  const first = event.subjects[0]
  if (!first) {
    return t('admin.audit.noSubjects')
  }
  return `${auditSubjectLabel(first.subjectKind)} · ${shortIdentifier(first.subjectId)}`
}

function actionSummary(event: AdminAuditEvent): string {
  return `${auditActionLabel(event.actionKind)} · ${humanizeToken(event.resultKind)}`
}

function eventMessage(event: AdminAuditEvent): string {
  return event.redactedMessage ?? event.internalMessage ?? actionSummary(event)
}
</script>

<template>
  <section class="rr-admin-ops-workbench">
    <div v-if="snapshot" class="rr-admin-ops-workbench__stack">
      <header class="rr-admin-ops-workbench__hero">
        <div class="rr-admin-ops-workbench__hero-copy">
          <h3>{{ headline.title }}</h3>
          <p>{{ headline.detail }}</p>
        </div>
        <span class="rr-status-pill" :class="headline.toneClass">
          {{ statusBadgeLabel(snapshot.state.degradedState) }}
        </span>
      </header>

      <div class="rr-admin-ops-workbench__summary">
        <article v-for="metric in metrics" :key="metric.key" class="rr-admin-ops-workbench__metric">
          <span>{{ metric.label }}</span>
          <strong>{{ metric.value }}</strong>
        </article>
        <article class="rr-admin-ops-workbench__metric rr-admin-ops-workbench__metric--wide">
          <span>{{ $t('admin.ops.generationTitle') }}</span>
          <strong>
            {{
              enumLabel(
                'admin.ops.generationStates',
                snapshot.state.knowledgeGenerationState,
                $t('admin.ops.noGeneration'),
              )
            }}
          </strong>
          <small
            >{{ $t('admin.ops.lastSyncTitle') }} ·
            {{ formatDateTime(snapshot.state.lastRecomputedAt) }}</small
          >
        </article>
      </div>

      <div class="rr-admin-ops-workbench__layout" :class="{ 'is-audit-focus': !hasWarnings }">
        <div v-if="hasWarnings" class="rr-admin-ops-workbench__panel">
          <div class="rr-admin-ops-workbench__panel-head">
            <h3>{{ $t('admin.ops.warningsTitle') }}</h3>
            <span>{{ activeWarnings.length }}</span>
          </div>

          <div class="rr-admin-ops-workbench__warning-list">
            <article
              v-for="warning in activeWarnings"
              :key="warning.id"
              class="rr-admin-ops-workbench__warning"
            >
              <div class="rr-admin-ops-workbench__warning-copy">
                <strong>{{ warningLabel(warning.warningKind) }}</strong>
                <span>{{ formatDateTime(warning.createdAt) }}</span>
              </div>
              <span class="rr-status-pill" :class="severityClass(warning.severity)">
                {{
                  enumLabel('admin.ops.severity', warning.severity, humanizeToken(warning.severity))
                }}
              </span>
            </article>
          </div>
        </div>

        <div class="rr-admin-ops-workbench__panel rr-admin-ops-workbench__panel--audit">
          <div class="rr-admin-ops-workbench__panel-head">
            <h3>{{ $t('admin.audit.title') }}</h3>
            <span>{{ visibleEvents.length }}</span>
          </div>

          <div v-if="visibleEvents.length" class="rr-admin-ops-workbench__audit-list">
            <article
              v-for="event in visibleEvents"
              :key="event.id"
              class="rr-admin-ops-workbench__audit"
            >
              <div class="rr-admin-ops-workbench__audit-meta">
                <strong>{{ actionSummary(event) }}</strong>
                <span>{{ formatDateTime(event.createdAt) }}</span>
              </div>
              <p>{{ eventMessage(event) }}</p>
              <div class="rr-admin-ops-workbench__audit-foot">
                <span>{{ subjectSummary(event) }}</span>
                <span v-if="event.actorPrincipalId">
                  {{
                    $t('admin.audit.actorLabel', { actor: shortIdentifier(event.actorPrincipalId) })
                  }}
                </span>
              </div>
            </article>
          </div>
          <p v-else class="rr-admin-ops-workbench__empty">
            {{ $t('admin.audit.empty') }}
          </p>
        </div>
      </div>
    </div>

    <p v-else class="rr-admin-ops-workbench__empty">
      {{ $t('admin.ops.unavailable') }}
    </p>
  </section>
</template>

<style scoped>
.rr-admin-ops-workbench {
  display: grid;
  gap: 0.8rem;
}

.rr-admin-ops-workbench__stack {
  display: grid;
  gap: 0.8rem;
  max-width: 1160px;
  margin-inline: auto;
}

.rr-admin-ops-workbench__hero,
.rr-admin-ops-workbench__summary,
.rr-admin-ops-workbench__panel {
  display: grid;
  padding: 0.85rem;
  border: 1px solid var(--rr-border-soft);
  border-radius: 16px;
  background: rgba(248, 250, 252, 0.68);
}

.rr-admin-ops-workbench__hero {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 0.8rem;
}

.rr-admin-ops-workbench__hero-copy {
  display: grid;
  gap: 0.3rem;
}

.rr-admin-ops-workbench__hero-copy h3,
.rr-admin-ops-workbench__panel-head h3 {
  margin: 0;
  font-size: 1rem;
  color: var(--rr-text-primary);
}

.rr-admin-ops-workbench__hero-copy p,
.rr-admin-ops-workbench__empty,
.rr-admin-ops-workbench__metric span,
.rr-admin-ops-workbench__warning-copy span,
.rr-admin-ops-workbench__audit-meta span,
.rr-admin-ops-workbench__audit-foot span,
.rr-admin-ops-workbench__metric small {
  color: var(--rr-text-secondary);
  font-size: 0.84rem;
  line-height: 1.5;
}

.rr-admin-ops-workbench__summary {
  grid-template-columns: repeat(5, minmax(0, 1fr));
  gap: 0.65rem;
}

.rr-admin-ops-workbench__metric {
  display: grid;
  gap: 0.2rem;
  padding: 0.72rem 0.78rem;
  border-radius: 12px;
  background: rgba(255, 255, 255, 0.72);
  border: 1px solid rgba(226, 232, 240, 0.86);
}

.rr-admin-ops-workbench__metric--wide {
  grid-column: span 2;
}

.rr-admin-ops-workbench__metric strong,
.rr-admin-ops-workbench__warning-copy strong,
.rr-admin-ops-workbench__audit-meta strong {
  color: var(--rr-text-primary);
}

.rr-admin-ops-workbench__metric strong {
  font-size: 1.2rem;
  line-height: 1;
}

.rr-admin-ops-workbench__layout {
  display: grid;
  grid-template-columns: minmax(0, 0.95fr) minmax(0, 1.05fr);
  gap: 0.8rem;
}

.rr-admin-ops-workbench__layout.is-audit-focus {
  grid-template-columns: minmax(0, 1fr);
  max-width: 980px;
}

.rr-admin-ops-workbench__panel {
  display: grid;
  gap: 0.7rem;
  align-content: start;
}

.rr-admin-ops-workbench__panel--audit {
  min-width: 0;
}

.rr-admin-ops-workbench__panel-head,
.rr-admin-ops-workbench__audit-meta,
.rr-admin-ops-workbench__audit-foot,
.rr-admin-ops-workbench__warning {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 0.75rem;
}

.rr-admin-ops-workbench__panel-head span {
  color: var(--rr-text-secondary);
  font-size: 0.88rem;
  font-weight: 700;
}

.rr-admin-ops-workbench__warning-list,
.rr-admin-ops-workbench__audit-list {
  display: grid;
  gap: 0.55rem;
}

.rr-admin-ops-workbench__warning,
.rr-admin-ops-workbench__audit {
  display: grid;
  gap: 0.35rem;
  padding: 0.72rem 0.78rem;
  border-radius: 12px;
  background: rgba(255, 255, 255, 0.72);
  border: 1px solid rgba(226, 232, 240, 0.86);
}

.rr-admin-ops-workbench__note {
  padding: 0.72rem 0.9rem;
  border-radius: 12px;
  border: 1px solid rgba(226, 232, 240, 0.86);
  background: rgba(248, 250, 252, 0.72);
}

.rr-admin-ops-workbench__warning-copy {
  display: grid;
  gap: 0.15rem;
}

.rr-admin-ops-workbench__audit p {
  margin: 0;
  color: var(--rr-text-secondary);
  font-size: 0.9rem;
  line-height: 1.45;
}

.rr-admin-ops-workbench__empty {
  margin: 0;
}

@media (max-width: 1024px) {
  .rr-admin-ops-workbench__summary {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }

  .rr-admin-ops-workbench__layout {
    grid-template-columns: 1fr;
  }
}

@media (max-width: 640px) {
  .rr-admin-ops-workbench__hero,
  .rr-admin-ops-workbench__summary,
  .rr-admin-ops-workbench__layout {
    grid-template-columns: 1fr;
  }

  .rr-admin-ops-workbench__hero,
  .rr-admin-ops-workbench__panel-head,
  .rr-admin-ops-workbench__audit-meta,
  .rr-admin-ops-workbench__audit-foot,
  .rr-admin-ops-workbench__warning {
    flex-direction: column;
  }
}
</style>
