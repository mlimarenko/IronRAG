<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import type { LibraryGraphCoverageSummary, LibraryReadinessSummary } from 'src/models/ui/documents'

const props = defineProps<{
  title: string
  description: string
  details?: string[]
  readinessSummary: LibraryReadinessSummary | null
  graphCoverage: LibraryGraphCoverageSummary | null
}>()

const { t } = useI18n()

const counts = computed(
  () =>
    props.readinessSummary?.documentCountsByReadiness ?? {
      processing: 0,
      readable: 0,
      graphSparse: 0,
      graphReady: 0,
      failed: 0,
    },
)

const summaryPills = computed(() => {
  const pills: { key: string; tone: string; label: string }[] = []

  if (counts.value.processing > 0) {
    pills.push({
      key: 'processing',
      tone: 'processing',
      label: t('graph.coverageCard.processing', { count: counts.value.processing }),
    })
  }

  if (counts.value.readable > 0) {
    pills.push({
      key: 'readable',
      tone: 'readable',
      label: t('graph.coverageCard.readable', { count: counts.value.readable }),
    })
  }

  if (counts.value.graphSparse > 0) {
    pills.push({
      key: 'graphSparse',
      tone: 'graph_sparse',
      label: t('graph.coverageCard.graphSparse', { count: counts.value.graphSparse }),
    })
  }

  if (counts.value.graphReady > 0) {
    pills.push({
      key: 'graphReady',
      tone: 'graph_ready',
      label: t('graph.coverageCard.graphReady', { count: counts.value.graphReady }),
    })
  }

  if (counts.value.failed > 0) {
    pills.push({
      key: 'failed',
      tone: 'failed',
      label: t('graph.coverageCard.failed', { count: counts.value.failed }),
    })
  }

  return pills
})

const graphCoverageFacts = computed(() => {
  if (!props.graphCoverage) {
    return []
  }

  const facts: string[] = []

  if (props.graphCoverage.typedFactDocumentCount > 0) {
    facts.push(
      t('graph.coverageCard.typedFacts', {
        count: props.graphCoverage.typedFactDocumentCount,
      }),
    )
  }

  if (props.graphCoverage.graphReadyDocumentCount > 0) {
    facts.push(
      t('graph.coverageCard.confirmed', {
        count: props.graphCoverage.graphReadyDocumentCount,
      }),
    )
  }

  return facts
})
</script>

<template>
  <section class="rr-graph-loading-shell" aria-live="polite">
    <div class="rr-graph-loading-shell__surface">
      <div class="rr-graph-loading-shell__copy">
        <span class="rr-graph-loading-shell__eyebrow">{{ $t('graph.coverageCard.eyebrow') }}</span>
        <h2>{{ props.title }}</h2>
        <p>{{ props.description }}</p>
      </div>

      <div class="rr-graph-loading-shell__status">
        <span class="rr-graph-loading-shell__pulse" aria-hidden="true" />
        <strong>{{ $t('graph.loading') }}</strong>
      </div>

      <div v-if="summaryPills.length" class="rr-graph-loading-shell__pills">
        <span
          v-for="pill in summaryPills"
          :key="pill.key"
          class="rr-status-pill"
          :class="`rr-status-pill--${pill.tone}`"
        >
          {{ pill.label }}
        </span>
      </div>

      <div class="rr-graph-loading-shell__progress" aria-hidden="true">
        <span class="is-wide" />
        <span class="is-medium" />
        <span class="is-short" />
      </div>

      <ul v-if="(props.details?.length ?? 0) > 0" class="rr-graph-loading-shell__details">
        <li v-for="detail in props.details" :key="detail">{{ detail }}</li>
      </ul>

      <div v-if="graphCoverageFacts.length" class="rr-graph-loading-shell__facts">
        <span v-for="fact in graphCoverageFacts" :key="fact">{{ fact }}</span>
      </div>
    </div>

    <div class="rr-graph-loading-shell__ghosts" aria-hidden="true">
      <section class="rr-graph-loading-shell__ghost rr-graph-loading-shell__ghost--controls">
        <span class="rr-graph-loading-shell__ghost-pill" />
        <span class="rr-graph-loading-shell__ghost-line is-wide" />
        <span class="rr-graph-loading-shell__ghost-line is-short" />
      </section>

      <section class="rr-graph-loading-shell__ghost rr-graph-loading-shell__ghost--inspector">
        <span class="rr-graph-loading-shell__ghost-pill" />
        <span class="rr-graph-loading-shell__ghost-block" />
        <span class="rr-graph-loading-shell__ghost-line is-wide" />
        <span class="rr-graph-loading-shell__ghost-line is-medium" />
      </section>
    </div>
  </section>
</template>

<style scoped lang="scss">
.rr-graph-loading-shell {
  display: grid;
  gap: 1rem;
  width: 100%;
}

.rr-graph-loading-shell__surface,
.rr-graph-loading-shell__ghost {
  position: relative;
  border: 1px solid rgba(191, 219, 254, 0.72);
  border-radius: 22px;
  background:
    linear-gradient(180deg, rgba(255, 255, 255, 0.98), rgba(248, 250, 252, 0.95)),
    rgba(255, 255, 255, 0.96);
  box-shadow: 0 18px 34px rgba(15, 23, 42, 0.08);
}

.rr-graph-loading-shell__surface {
  display: grid;
  gap: 0.9rem;
  padding: 1.2rem 1.25rem;
}

.rr-graph-loading-shell__copy {
  display: grid;
  gap: 0.4rem;
}

.rr-graph-loading-shell__eyebrow {
  color: rgba(71, 85, 105, 0.72);
  font-size: 0.72rem;
  font-weight: 700;
  letter-spacing: 0.08em;
  text-transform: uppercase;
}

.rr-graph-loading-shell__copy h2 {
  margin: 0;
  color: rgba(15, 23, 42, 0.96);
  font-size: 1.22rem;
  line-height: 1.15;
  letter-spacing: -0.03em;
}

.rr-graph-loading-shell__copy p {
  max-width: 52rem;
  margin: 0;
  color: rgba(51, 65, 85, 0.82);
  font-size: 0.92rem;
  line-height: 1.55;
}

.rr-graph-loading-shell__status {
  display: inline-flex;
  align-items: center;
  gap: 0.55rem;
  color: rgba(30, 64, 175, 0.94);
}

.rr-graph-loading-shell__status strong {
  font-size: 0.82rem;
  font-weight: 700;
}

.rr-graph-loading-shell__pulse {
  width: 0.62rem;
  height: 0.62rem;
  border-radius: 999px;
  background: linear-gradient(135deg, #2563eb, #4f46e5);
  box-shadow: 0 0 0 0 rgba(79, 70, 229, 0.18);
  animation: rr-graph-loading-shell-pulse 1.8s ease-out infinite;
}

.rr-graph-loading-shell__pills,
.rr-graph-loading-shell__facts {
  display: flex;
  flex-wrap: wrap;
  gap: 0.5rem;
}

.rr-graph-loading-shell__progress {
  display: grid;
  gap: 0.55rem;
}

.rr-graph-loading-shell__progress span,
.rr-graph-loading-shell__ghost-line,
.rr-graph-loading-shell__ghost-pill,
.rr-graph-loading-shell__ghost-block {
  display: block;
  border-radius: 999px;
  background: linear-gradient(
    90deg,
    rgba(226, 232, 240, 0.76) 18%,
    rgba(241, 245, 249, 0.96) 36%,
    rgba(226, 232, 240, 0.76) 54%
  );
  background-size: 220% 100%;
  animation: rr-graph-loading-shell-shimmer 1.8s ease-in-out infinite;
}

.rr-graph-loading-shell__progress .is-wide,
.rr-graph-loading-shell__ghost-line.is-wide {
  width: min(100%, 29rem);
}

.rr-graph-loading-shell__progress .is-medium,
.rr-graph-loading-shell__ghost-line.is-medium {
  width: min(100%, 20rem);
}

.rr-graph-loading-shell__progress .is-short,
.rr-graph-loading-shell__ghost-line.is-short {
  width: min(100%, 12rem);
}

.rr-graph-loading-shell__progress span,
.rr-graph-loading-shell__ghost-line {
  height: 0.74rem;
}

.rr-graph-loading-shell__details {
  display: grid;
  gap: 0.45rem;
  margin: 0;
  padding: 0 0 0 1.15rem;
  color: rgba(71, 85, 105, 0.84);
  font-size: 0.82rem;
  line-height: 1.45;
}

.rr-graph-loading-shell__facts span {
  display: inline-flex;
  align-items: center;
  min-height: 1.9rem;
  padding: 0 0.72rem;
  border-radius: 999px;
  background: rgba(241, 245, 249, 0.88);
  color: rgba(51, 65, 85, 0.88);
  font-size: 0.78rem;
  font-weight: 600;
}

.rr-graph-loading-shell__ghosts {
  display: grid;
  grid-template-columns: minmax(0, 1.4fr) minmax(18rem, 0.92fr);
  gap: 0.9rem;
}

.rr-graph-loading-shell__ghost {
  display: grid;
  gap: 0.75rem;
  padding: 1rem 1.05rem;
}

.rr-graph-loading-shell__ghost-pill {
  width: 7.6rem;
  height: 1.3rem;
}

.rr-graph-loading-shell__ghost-block {
  width: 100%;
  height: 4.8rem;
  border-radius: 18px;
}

@keyframes rr-graph-loading-shell-pulse {
  0% {
    box-shadow: 0 0 0 0 rgba(79, 70, 229, 0.22);
  }

  70% {
    box-shadow: 0 0 0 10px rgba(79, 70, 229, 0);
  }

  100% {
    box-shadow: 0 0 0 0 rgba(79, 70, 229, 0);
  }
}

@keyframes rr-graph-loading-shell-shimmer {
  0% {
    background-position: 100% 50%;
  }

  100% {
    background-position: 0% 50%;
  }
}

@media (max-width: 860px) {
  .rr-graph-loading-shell__ghosts {
    grid-template-columns: minmax(0, 1fr);
  }

  .rr-graph-loading-shell__surface,
  .rr-graph-loading-shell__ghost {
    border-radius: 18px;
  }
}

@media (max-width: 640px) {
  .rr-graph-loading-shell__surface,
  .rr-graph-loading-shell__ghost {
    padding: 0.95rem;
  }

  .rr-graph-loading-shell__copy h2 {
    font-size: 1.08rem;
  }

  .rr-graph-loading-shell__copy p,
  .rr-graph-loading-shell__details {
    font-size: 0.82rem;
  }
}
</style>
