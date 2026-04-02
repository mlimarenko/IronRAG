<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import StatusBadge from 'src/components/design-system/StatusBadge.vue'
import type { WebIngestRunSummary, WebRunState } from 'src/models/ui/documents'

const props = withDefaults(
  defineProps<{
    activeRuns: WebIngestRunSummary[]
    recentRuns?: WebIngestRunSummary[]
    cancelingRunId?: string | null
  }>(),
  {
    recentRuns: () => [],
    cancelingRunId: null,
  },
)

const emit = defineEmits<{
  openRun: [runId: string]
  cancelRun: [runId: string]
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

function runLabel(run: WebIngestRunSummary): string {
  try {
    const parsed = new URL(run.seedUrl)
    const path = parsed.pathname === '/' ? '' : parsed.pathname
    return `${parsed.host}${path}`
  } catch {
    return run.seedUrl
  }
}

function canCancel(run: WebIngestRunSummary): boolean {
  return ['accepted', 'discovering', 'processing'].includes(run.runState)
}

function inFlightCount(run: WebIngestRunSummary): number {
  return run.counts.queued + run.counts.processing
}

function runSeedKey(seedUrl: string): string {
  try {
    const parsed = new URL(seedUrl)
    const path = parsed.pathname.replace(/\/+$/, '') || '/'
    return `${parsed.origin}${path}`
  } catch {
    return seedUrl.trim().toLowerCase()
  }
}

const visibleRuns = computed(() => {
  const ordered = [...props.activeRuns, ...(props.recentRuns ?? [])]
  const seenSeeds = new Set<string>()
  const items: WebIngestRunSummary[] = []
  for (const run of ordered) {
    const seedKey = runSeedKey(run.seedUrl)
    if (seenSeeds.has(seedKey)) {
      continue
    }
    seenSeeds.add(seedKey)
    items.push(run)
  }
  return items.slice(0, 4)
})

const stripTitle = computed(() =>
  props.activeRuns.length > 0
    ? t('documents.webRuns.activity.titleActive', { count: props.activeRuns.length })
    : t('documents.webRuns.activity.titleRecent'),
)

const compactMode = computed(() => visibleRuns.value.length <= 1)
</script>

<template>
  <section
    v-if="visibleRuns.length > 0"
    class="rr-web-run-activity"
    :class="{ 'is-compact': compactMode }"
    role="status"
    aria-live="polite"
  >
    <div v-if="!compactMode" class="rr-web-run-activity__head">
      <div class="rr-web-run-activity__copy">
        <strong>{{ stripTitle }}</strong>
        <span>{{ $t('documents.webRuns.activity.subtitle') }}</span>
      </div>
    </div>

    <div class="rr-web-run-activity__list">
      <article
        v-for="run in visibleRuns"
        :key="run.runId"
        class="rr-web-run-activity__item"
        :data-state="run.runState"
      >
        <button type="button" class="rr-web-run-activity__open" @click="emit('openRun', run.runId)">
          <div class="rr-web-run-activity__identity">
            <strong>{{ runLabel(run) }}</strong>
            <span>{{
              $t('documents.webRuns.activity.pagesCoverage', {
                processed: run.counts.processed,
                discovered: run.counts.discovered,
              })
            }}</span>
          </div>

          <div class="rr-web-run-activity__meta">
            <StatusBadge :kind="stateTone(run.runState)" :label="stateLabel(run.runState)" />
            <span v-if="inFlightCount(run) > 0" class="rr-web-run-activity__inflight">
              {{
                $t('documents.webRuns.activity.pagesInFlight', {
                  count: run.counts.queued + run.counts.processing,
                })
              }}
            </span>
          </div>
        </button>

        <button
          v-if="canCancel(run)"
          type="button"
          class="rr-button rr-button--ghost rr-button--tiny rr-web-run-activity__cancel"
          :disabled="props.cancelingRunId === run.runId"
          @click="emit('cancelRun', run.runId)"
        >
          {{
            props.cancelingRunId === run.runId
              ? $t('documents.webRuns.activity.canceling')
              : $t('documents.webRuns.actions.cancel')
          }}
        </button>
      </article>
    </div>
  </section>
</template>

<style scoped lang="scss">
.rr-web-run-activity {
  display: grid;
  gap: 4px;
  padding: 5px 6px;
  border: 1px solid rgba(191, 219, 254, 0.75);
  border-radius: 10px;
  background:
    radial-gradient(circle at top right, rgba(14, 165, 233, 0.06), transparent 28%),
    rgba(247, 250, 255, 0.92);
}

.rr-web-run-activity.is-compact {
  gap: 0;
  background: rgba(248, 250, 252, 0.82);
}

.rr-web-run-activity__copy {
  display: grid;
  gap: 2px;
}

.rr-web-run-activity__copy strong {
  color: var(--rr-text-primary);
  font-size: 0.8rem;
}

.rr-web-run-activity__copy span {
  color: var(--rr-text-secondary);
  font-size: 0.69rem;
  line-height: 1.35;
}

.rr-web-run-activity__list {
  display: grid;
  gap: 4px;
}

.rr-web-run-activity__item {
  display: grid;
  grid-template-columns: minmax(0, 1fr) auto;
  gap: 6px;
  align-items: center;
  padding: 4px 6px;
  border: 1px solid rgba(226, 232, 240, 0.86);
  border-radius: 9px;
  background: rgba(255, 255, 255, 0.86);
}

.rr-web-run-activity.is-compact .rr-web-run-activity__item {
  padding: 3px 5px;
}

.rr-web-run-activity__open {
  display: grid;
  grid-template-columns: minmax(0, 1fr) auto;
  gap: 8px;
  align-items: center;
  width: 100%;
  padding: 0;
  border: none;
  background: transparent;
  text-align: left;
  cursor: pointer;
}

.rr-web-run-activity__identity {
  display: grid;
  gap: 3px;
  min-width: 0;
}

.rr-web-run-activity__identity strong {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  color: var(--rr-text-primary);
  font-size: 0.78rem;
  line-height: 1.35;
}

.rr-web-run-activity__identity span,
.rr-web-run-activity__inflight {
  color: var(--rr-text-secondary);
  font-size: 0.68rem;
  line-height: 1.32;
}

.rr-web-run-activity__meta {
  display: grid;
  justify-items: end;
  gap: 4px;
}

.rr-web-run-activity__cancel {
  min-height: 28px;
}

@media (min-width: 1180px) {
  .rr-web-run-activity__list {
    grid-template-columns: repeat(auto-fit, minmax(260px, 1fr));
  }
}

@media (max-width: 720px) {
  .rr-web-run-activity__item,
  .rr-web-run-activity__open {
    grid-template-columns: 1fr;
  }

  .rr-web-run-activity__meta {
    justify-items: start;
  }

  .rr-web-run-activity__cancel {
    justify-self: start;
  }
}

@media (min-width: 900px) {
  .rr-web-run-activity__copy span {
    display: none;
  }
}
</style>
