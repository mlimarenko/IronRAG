<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import type { LibraryGraphCoverageSummary, LibraryReadinessSummary } from 'src/models/ui/documents'

const props = defineProps<{
  tone: 'loading' | 'empty' | 'sparse' | 'failed'
  title: string
  description: string
  details?: string[]
  readinessSummary: LibraryReadinessSummary | null
  graphCoverage: LibraryGraphCoverageSummary | null
  actionLabel?: string | null
}>()

const emit = defineEmits<{
  action: []
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
  const pills: {
    key: string
    tone: 'processing' | 'readable' | 'graph_sparse' | 'graph_ready' | 'failed'
    label: string
  }[] = []

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
  <section class="rr-graph-coverage-card" :class="`is-${props.tone}`">
    <div class="rr-graph-coverage-card__copy">
      <span class="rr-graph-coverage-card__eyebrow">{{ $t('graph.coverageCard.eyebrow') }}</span>
      <h3>{{ props.title }}</h3>
      <p>{{ props.description }}</p>
    </div>

    <div v-if="summaryPills.length" class="rr-graph-coverage-card__pills">
      <span
        v-for="pill in summaryPills"
        :key="pill.key"
        class="rr-status-pill"
        :class="`rr-status-pill--${pill.tone}`"
      >
        {{ pill.label }}
      </span>
    </div>

    <ul v-if="(props.details?.length ?? 0) > 0" class="rr-graph-coverage-card__details">
      <li v-for="detail in props.details" :key="detail">
        {{ detail }}
      </li>
    </ul>

    <div v-if="graphCoverageFacts.length" class="rr-graph-coverage-card__facts">
      <span v-for="fact in graphCoverageFacts" :key="fact">{{ fact }}</span>
    </div>

    <div v-if="props.actionLabel" class="rr-graph-coverage-card__actions">
      <button type="button" class="rr-button rr-button--primary" @click="emit('action')">
        {{ props.actionLabel }}
      </button>
    </div>
  </section>
</template>

<style scoped lang="scss">
.rr-graph-coverage-card {
  position: relative;
  display: grid;
  gap: 0.9rem;
  width: min(32rem, calc(100vw - 3rem));
  padding: 1.2rem 1.25rem;
  border: 1px solid rgba(191, 219, 254, 0.85);
  border-radius: 1.35rem;
  background: linear-gradient(180deg, rgba(255, 255, 255, 0.98), rgba(248, 250, 252, 0.97));
  box-shadow:
    0 18px 40px rgba(15, 23, 42, 0.08),
    inset 0 1px 0 rgba(255, 255, 255, 0.92);
}

.rr-graph-coverage-card::before {
  content: '';
  position: absolute;
  inset: 0 auto 0 0;
  width: 5px;
  border-radius: 999px;
  background: linear-gradient(180deg, rgba(96, 165, 250, 0.9), rgba(99, 102, 241, 0.55));
}

.rr-graph-coverage-card.is-loading {
  border-color: rgba(147, 197, 253, 0.88);
}

.rr-graph-coverage-card.is-sparse {
  border-color: rgba(125, 211, 252, 0.9);
}

.rr-graph-coverage-card.is-sparse::before {
  background: linear-gradient(180deg, rgba(34, 197, 94, 0.82), rgba(14, 165, 233, 0.6));
}

.rr-graph-coverage-card.is-failed {
  border-color: rgba(251, 146, 60, 0.78);
  background: linear-gradient(180deg, rgba(255, 251, 245, 0.98), rgba(255, 247, 237, 0.97));
}

.rr-graph-coverage-card.is-failed,
.rr-graph-coverage-card.is-empty {
  width: min(36rem, calc(100vw - 3rem));
}

.rr-graph-coverage-card.is-failed::before {
  background: linear-gradient(180deg, rgba(249, 115, 22, 0.92), rgba(234, 88, 12, 0.55));
}

.rr-graph-coverage-card__copy {
  display: grid;
  gap: 0.45rem;
}

.rr-graph-coverage-card__eyebrow {
  color: rgba(71, 85, 105, 0.72);
  font-size: 0.72rem;
  font-weight: 700;
  letter-spacing: 0.08em;
  text-transform: uppercase;
}

.rr-graph-coverage-card__copy h3 {
  margin: 0;
  color: rgba(15, 23, 42, 0.96);
  font-size: 1.08rem;
  line-height: 1.2;
}

.rr-graph-coverage-card__copy p {
  margin: 0;
  color: rgba(51, 65, 85, 0.82);
  font-size: 0.92rem;
  line-height: 1.52;
}

.rr-graph-coverage-card__pills,
.rr-graph-coverage-card__facts {
  display: flex;
  flex-wrap: wrap;
  gap: 0.5rem;
}

.rr-graph-coverage-card__pills {
  padding-bottom: 0.1rem;
}

.rr-graph-coverage-card__facts span {
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

.rr-graph-coverage-card__details {
  display: grid;
  gap: 0.45rem;
  margin: 0;
  padding: 0.85rem 0.95rem 0.85rem 1.55rem;
  border-radius: 1rem;
  background: rgba(248, 250, 252, 0.76);
  border: 1px solid rgba(191, 219, 254, 0.42);
  color: rgba(51, 65, 85, 0.8);
  font-size: 0.84rem;
  line-height: 1.45;
}

.rr-graph-coverage-card__actions {
  display: flex;
  justify-content: flex-start;
  padding-top: 0.15rem;
}

@media (min-width: 1400px) {
  .rr-graph-coverage-card {
    width: min(40rem, calc(100vw - 3rem));
    padding: 1.35rem 1.4rem;
  }

  .rr-graph-coverage-card.is-failed,
  .rr-graph-coverage-card.is-empty {
    width: min(44rem, calc(100vw - 3rem));
  }
}

@media (max-width: 760px) {
  .rr-graph-coverage-card {
    width: min(100%, calc(100vw - 1.25rem));
    gap: 0.8rem;
    padding: 1rem;
    border-radius: 1.1rem;
  }

  .rr-graph-coverage-card__copy h3 {
    font-size: 1rem;
  }

  .rr-graph-coverage-card__copy p,
  .rr-graph-coverage-card__details {
    font-size: 0.84rem;
  }
}
</style>
