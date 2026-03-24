<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { useDisplayFormatters } from 'src/composables/useDisplayFormatters'
import type { AdminOpsLibrarySnapshot } from 'src/models/ui/admin'

const props = defineProps<{
  snapshot: AdminOpsLibrarySnapshot | null
}>()

const { t } = useI18n()
const { enumLabel, formatDateTime, humanizeToken, statusBadgeLabel } = useDisplayFormatters()

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
</script>

<template>
  <section class="rr-admin-ops-card">
    <div
      v-if="snapshot"
      class="rr-admin-ops-card__stack"
    >
      <div class="rr-admin-ops-card__metrics">
        <article
          v-for="metric in metrics"
          :key="metric.key"
          class="rr-admin-ops-card__metric"
        >
          <span>{{ metric.label }}</span>
          <strong>{{ metric.value }}</strong>
        </article>
      </div>

      <div class="rr-admin-ops-card__health">
        <div>
          <span>{{ $t('admin.ops.healthTitle') }}</span>
          <strong>{{ statusBadgeLabel(snapshot.state.degradedState) }}</strong>
        </div>
        <div>
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
        </div>
        <div>
          <span>{{ $t('admin.ops.lastSyncTitle') }}</span>
          <strong>{{ formatDateTime(snapshot.state.lastRecomputedAt) }}</strong>
        </div>
      </div>

      <div class="rr-admin-ops-card__warnings">
        <div class="rr-admin-ops-card__warnings-head">
          <h3>{{ $t('admin.ops.warningsTitle') }}</h3>
          <span>{{ activeWarnings.length }}</span>
        </div>

        <div
          v-if="activeWarnings.length"
          class="rr-admin-ops-card__warning-list"
        >
          <article
            v-for="warning in activeWarnings"
            :key="warning.id"
            class="rr-admin-ops-card__warning"
          >
            <div class="rr-admin-ops-card__warning-main">
              <strong>{{ warningLabel(warning.warningKind) }}</strong>
              <span>{{ formatDateTime(warning.createdAt) }}</span>
            </div>
            <span
              class="rr-status-pill"
              :class="severityClass(warning.severity)"
            >
              {{ enumLabel('admin.ops.severity', warning.severity, humanizeToken(warning.severity)) }}
            </span>
          </article>
        </div>

        <p
          v-else
          class="rr-admin-ops-card__empty"
        >
          {{ $t('admin.ops.warningsEmpty') }}
        </p>
      </div>
    </div>

    <p
      v-else
      class="rr-admin-ops-card__empty"
    >
      {{ $t('admin.ops.unavailable') }}
    </p>
  </section>
</template>

<style scoped>
.rr-admin-ops-card {
  display: grid;
  gap: 0.8rem;
}

.rr-admin-ops-card__stack {
  display: grid;
  gap: 0.8rem;
}

.rr-admin-ops-card__metrics {
  display: grid;
  grid-template-columns: repeat(4, minmax(0, 1fr));
  gap: 0.7rem;
}

.rr-admin-ops-card__metric,
.rr-admin-ops-card__health,
.rr-admin-ops-card__warnings {
  padding: 0.9rem;
  border: 1px solid var(--rr-border-muted);
  border-radius: 16px;
  background: rgba(248, 250, 252, 0.7);
}

.rr-admin-ops-card__metric {
  display: grid;
  gap: 0.25rem;
}

.rr-admin-ops-card__metric span,
.rr-admin-ops-card__health span,
.rr-admin-ops-card__warning-main span {
  font-size: 0.84rem;
  color: var(--rr-text-secondary);
}

.rr-admin-ops-card__metric strong,
.rr-admin-ops-card__health strong,
.rr-admin-ops-card__warning-main strong {
  color: var(--rr-text-primary);
}

.rr-admin-ops-card__metric strong {
  font-size: 1.45rem;
  line-height: 1;
}

.rr-admin-ops-card__health {
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  gap: 0.75rem;
}

.rr-admin-ops-card__health > div {
  display: grid;
  gap: 0.25rem;
}

.rr-admin-ops-card__warnings {
  display: grid;
  gap: 0.75rem;
}

.rr-admin-ops-card__warnings-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 0.75rem;
}

.rr-admin-ops-card__warnings-head h3 {
  margin: 0;
  font-size: 1rem;
  color: var(--rr-text-primary);
}

.rr-admin-ops-card__warnings-head span {
  color: var(--rr-text-secondary);
  font-size: 0.88rem;
  font-weight: 700;
}

.rr-admin-ops-card__warning-list {
  display: grid;
  gap: 0.55rem;
}

.rr-admin-ops-card__warning {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 0.75rem;
}

.rr-admin-ops-card__warning-main {
  display: grid;
  gap: 0.15rem;
}

.rr-admin-ops-card__empty {
  margin: 0;
  color: var(--rr-text-secondary);
  font-size: 0.94rem;
  line-height: 1.5;
}

@media (max-width: 1024px) {
  .rr-admin-ops-card__metrics,
  .rr-admin-ops-card__health {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }
}

@media (max-width: 640px) {
  .rr-admin-ops-card__metrics,
  .rr-admin-ops-card__health {
    grid-template-columns: 1fr;
  }

  .rr-admin-ops-card__warning {
    flex-direction: column;
  }
}
</style>
