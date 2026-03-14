<script setup lang="ts">
import { computed, ref } from 'vue'

import PageSection from 'src/components/shell/PageSection.vue'
import StatusBadge from 'src/components/shell/StatusBadge.vue'
import EmptyStateCard from 'src/components/state/EmptyStateCard.vue'

interface GraphProductMetric {
  label: string
  value: string
  tone?: 'default' | 'good' | 'warning'
}

interface GraphSearchResult {
  id: string
  title: string
  kind: string
  summary: string
  evidence: string[]
}

interface GraphRelation {
  from: string
  relation: string
  to: string
}

const searchQuery = ref('')
const selectedResultId = ref<string | null>(null)

const productMetrics = computed<GraphProductMetric[]>(() => [
  {
    label: 'Entity coverage',
    value: 'Awaiting first retrieval run',
    tone: 'warning',
  },
  {
    label: 'Relation freshness',
    value: 'No graph snapshots yet',
    tone: 'warning',
  },
  {
    label: 'Operator posture',
    value: 'Workspace ready for graph signals',
    tone: 'good',
  },
])

const graphSummary = computed(() => ({
  status: 'Preview',
  headline:
    'Use Graph to inspect entity coverage, relation visibility, and retrieval-linked evidence as graph data becomes available.',
  body: 'This workspace already gives operators a clear map of what graph evidence exists today, what comes from retrieval details, and which graph records are still waiting on backend support.',
  highlights: [
    'Retrieval detail already captures references, matched chunks, and raw debug payloads.',
    'Search and detail panels stay explicit about which graph records are available right now.',
    'As dedicated graph APIs come online, this view can switch from guidance to live entity and relation inspection without changing the workflow.',
  ],
}))

const demoSearchResults = computed<GraphSearchResult[]>(() => [
  {
    id: 'retrieval-signals',
    title: 'Retrieval signals',
    kind: 'Available now',
    summary:
      'Current graph-adjacent data comes from retrieval references, matched chunks, and debug JSON captured per run.',
    evidence: ['references[]', 'matched_chunk_ids[]', 'debug_json'],
  },
  {
    id: 'entity-index',
    title: 'Entity index',
    kind: 'Awaiting backend data',
    summary:
      'Entity search is ready to display results, but no backend endpoint exposes canonical entities yet.',
    evidence: ['Needs graph entity list API', 'Needs project-scoped indexing'],
  },
  {
    id: 'relation-inspector',
    title: 'Relation inspector',
    kind: 'Awaiting backend data',
    summary:
      'Relation detail is prepared for operator review, but relation tuples are not returned by the platform today.',
    evidence: ['Needs relation edges API', 'Needs provenance payload'],
  },
])

const filteredResults = computed(() => {
  const query = searchQuery.value.trim().toLowerCase()
  if (!query) {
    return demoSearchResults.value
  }

  return demoSearchResults.value.filter((item) => {
    return [item.title, item.kind, item.summary, ...item.evidence].some((value) =>
      value.toLowerCase().includes(query),
    )
  })
})

const selectedResult = computed(() => {
  const fromSelection = demoSearchResults.value.find((item) => item.id === selectedResultId.value)
  if (fromSelection) {
    return fromSelection
  }

  return filteredResults.value[0] ?? null
})

const detailRelations = computed<GraphRelation[]>(() => {
  const selectedId = selectedResult.value.id

  switch (selectedId) {
    case 'retrieval-signals':
      return [
        { from: 'Retrieval run', relation: 'records', to: 'References' },
        { from: 'Retrieval run', relation: 'matches', to: 'Chunk IDs' },
        { from: 'Retrieval run', relation: 'captures', to: 'Debug payload' },
      ]
    default:
      return []
  }
})

const hasSearchResults = computed(() => filteredResults.value.length > 0)
const hasDetailContent = computed(() => Boolean(selectedResult.value))
</script>

<template>
  <PageSection
    eyebrow="Knowledge graph"
    title="Graph"
    description="Inspect graph readiness, search graph concepts, and review which entities or relations are already visible versus still waiting on backend support."
    status="In progress"
    status-label="Preview"
  >
    <section class="hero card">
      <div class="hero__copy">
        <p class="hero__eyebrow rr-kicker">{{ graphSummary.status }}</p>
        <h2>{{ graphSummary.headline }}</h2>
        <p>{{ graphSummary.body }}</p>
      </div>

      <div class="hero__metrics">
        <article
          v-for="metric in productMetrics"
          :key="metric.label"
          class="metric-card"
          :data-tone="metric.tone ?? 'default'"
        >
          <span class="metric-card__label">{{ metric.label }}</span>
          <strong>{{ metric.value }}</strong>
        </article>
      </div>

      <ul class="hero__highlights">
        <li v-for="highlight in graphSummary.highlights" :key="highlight">
          {{ highlight }}
        </li>
      </ul>
    </section>

    <div class="workspace-grid">
      <article class="card workspace-panel">
        <div class="panel-header">
          <div>
            <p class="rr-kicker">Current coverage</p>
            <h3>Graph summary</h3>
            <p class="panel-subtitle">
              What RustRAG can show today, what comes from retrieval, and where backend graph
              records are still missing.
            </p>
          </div>
          <StatusBadge status="Preview" />
        </div>

        <div class="summary-list">
          <article class="summary-row">
            <span class="summary-row__label">Current source of truth</span>
            <strong>Retrieval run detail and graph-backed metadata</strong>
          </article>
          <article class="summary-row">
            <span class="summary-row__label">Available graph evidence</span>
            <strong>References, matched chunks, debug payload</strong>
          </article>
          <article class="summary-row">
            <span class="summary-row__label">Still unavailable</span>
            <strong>Entity list, relation edges, provenance-rich graph API</strong>
          </article>
        </div>
      </article>

      <article class="card workspace-panel">
        <div class="panel-header panel-header--stacked">
          <div>
            <p class="rr-kicker">Discovery</p>
            <h3>Graph search</h3>
            <p class="panel-subtitle">
              Search across available graph signals and the backend capabilities this workspace is
              still waiting for.
            </p>
          </div>
          <label class="search-field">
            <span class="search-field__label">Search graph concepts</span>
            <input
              v-model="searchQuery"
              type="text"
              placeholder="Search entities, relations, retrieval, debug…"
            />
          </label>
        </div>

        <div v-if="hasSearchResults" class="search-results">
          <button
            v-for="item in filteredResults"
            :key="item.id"
            type="button"
            class="search-result"
            :data-active="selectedResult?.id === item.id"
            @click="selectedResultId = item.id"
          >
            <div class="search-result__meta">
              <span class="search-result__kind">{{ item.kind }}</span>
              <strong>{{ item.title }}</strong>
            </div>
            <p>{{ item.summary }}</p>
          </button>
        </div>

        <EmptyStateCard
          v-else
          title="No graph matches yet"
          message="No graph concepts on this page match that search yet."
          hint="Try broader terms like retrieval, relation, entity, or debug. This search becomes richer as graph APIs and indexed records arrive."
        />
      </article>
    </div>

    <article class="card workspace-panel detail-panel">
      <div class="panel-header">
        <div>
          <p class="rr-kicker">Detail</p>
          <h3>Graph detail</h3>
          <p class="panel-subtitle">
            Review the selected concept, what evidence is available now, and whether live relation
            records can already be inspected.
          </p>
        </div>
      </div>

      <template v-if="hasDetailContent && selectedResult">
        <div class="detail-header">
          <div>
            <p class="detail-header__kind">{{ selectedResult.kind }}</p>
            <h4>{{ selectedResult.title }}</h4>
          </div>
          <StatusBadge
            :status="detailRelations.length ? 'Ready' : 'Blocked'"
            :label="detailRelations.length ? 'Inspectable' : 'Waiting on API'"
          />
        </div>

        <p class="detail-summary">{{ selectedResult.summary }}</p>

        <div class="detail-grid">
          <section class="detail-card">
            <h5>Available evidence</h5>
            <ul>
              <li v-for="evidence in selectedResult.evidence" :key="evidence">
                {{ evidence }}
              </li>
            </ul>
          </section>

          <section class="detail-card">
            <h5>Relation view</h5>
            <div v-if="detailRelations.length" class="relation-list">
              <article
                v-for="relation in detailRelations"
                :key="`${relation.from}-${relation.relation}-${relation.to}`"
                class="relation-row"
              >
                <strong>{{ relation.from }}</strong>
                <span>{{ relation.relation }}</span>
                <strong>{{ relation.to }}</strong>
              </article>
            </div>
            <EmptyStateCard
              v-else
              title="No live relation edges yet"
              message="The backend does not expose canonical relation tuples for this concept yet."
              hint="As soon as graph APIs provide relation data, this panel should show provenance-rich edges and neighbors instead of explanatory text."
            />
          </section>
        </div>
      </template>

      <EmptyStateCard
        v-else
        title="No graph detail selected"
        message="Pick a graph concept from search to review available evidence and current backend coverage."
        hint="This keeps the page actionable without inventing entities or relations that do not exist yet."
      />
    </article>
  </PageSection>
</template>

<style scoped>
.card {
  padding: var(--rr-space-6);
  border: 1px solid var(--rr-color-border-subtle);
  border-radius: var(--rr-radius-md);
  background: var(--rr-color-bg-surface);
  box-shadow: var(--rr-shadow-sm);
}

.hero,
.workspace-panel,
.hero__copy,
.hero__metrics,
.workspace-grid,
.detail-grid,
.summary-list,
.search-results,
.relation-list {
  display: grid;
}

.hero,
.workspace-panel {
  gap: var(--rr-space-4);
}

.hero__copy {
  gap: var(--rr-space-3);
  max-width: 76ch;
}

.hero__copy h2,
.hero__copy p,
.hero__highlights,
.hero__highlights li,
.panel-header h3,
.panel-subtitle,
.summary-row,
.summary-row__label,
.search-result p,
.detail-summary,
.detail-card h5,
.detail-card ul,
.detail-card li,
.detail-header h4,
.detail-header__kind {
  margin: 0;
}

.hero__eyebrow,
.panel-subtitle,
.summary-row__label,
.search-field__label,
.search-result__kind,
.detail-header__kind {
  color: var(--rr-color-text-muted);
}

.hero__eyebrow,
.search-result__kind,
.detail-header__kind {
  font-size: 0.8rem;
  font-weight: 700;
  letter-spacing: 0.08em;
  text-transform: uppercase;
}

.hero {
  background:
    radial-gradient(circle at top right, rgb(59 130 246 / 0.12), transparent 28%),
    var(--rr-color-bg-surface);
}

.hero__metrics,
.workspace-grid,
.detail-grid {
  gap: var(--rr-space-4);
}

.hero__metrics {
  grid-template-columns: repeat(auto-fit, minmax(220px, 1fr));
}

.metric-card,
.summary-row,
.detail-card,
.search-result,
.relation-row {
  border-radius: var(--rr-radius-sm);
}

.metric-card {
  display: grid;
  gap: var(--rr-space-2);
  padding: var(--rr-space-4);
  border: 1px solid var(--rr-color-border-subtle);
  background: var(--rr-color-bg-surface-muted);
}

.metric-card[data-tone='warning'] {
  background: var(--rr-color-warning-50);
  border-color: rgb(217 119 6 / 0.24);
}

.metric-card[data-tone='good'] {
  background: var(--rr-color-success-50);
  border-color: rgb(22 163 74 / 0.22);
}

.metric-card__label {
  color: var(--rr-color-text-muted);
  font-size: 0.92rem;
}

.hero__highlights {
  padding-left: 18px;
  color: var(--rr-color-text-secondary);
  gap: var(--rr-space-2);
}

.workspace-grid {
  grid-template-columns: repeat(auto-fit, minmax(320px, 1fr));
}

.panel-header {
  display: flex;
  justify-content: space-between;
  gap: var(--rr-space-4);
  align-items: flex-start;
}

.panel-header--stacked {
  flex-direction: column;
}

.summary-list,
.search-results,
.relation-list {
  gap: var(--rr-space-3);
}

.summary-row {
  display: grid;
  gap: 6px;
  padding: var(--rr-space-4);
  background: var(--rr-color-bg-surface-muted);
}

.search-field {
  display: grid;
  gap: 6px;
  width: 100%;
}

.search-field input {
  width: 100%;
  padding: 11px 13px;
  border: 1px solid var(--rr-color-border-strong);
  border-radius: 12px;
  font: inherit;
  background: #fff;
}

.search-result {
  display: grid;
  gap: var(--rr-space-2);
  width: 100%;
  padding: var(--rr-space-4);
  border: 1px solid var(--rr-color-border-subtle);
  text-align: left;
  background: #fff;
  cursor: pointer;
  transition:
    border-color var(--rr-motion-base),
    box-shadow var(--rr-motion-base),
    transform var(--rr-motion-base);
}

.search-result:hover,
.search-result[data-active='true'] {
  border-color: var(--rr-color-border-focus);
  box-shadow: var(--rr-shadow-sm);
  transform: translateY(-1px);
}

.search-result[data-active='true'] {
  background: var(--rr-color-accent-50);
}

.search-result__meta {
  display: grid;
  gap: 4px;
}

.detail-panel {
  gap: var(--rr-space-5);
}

.detail-header {
  display: flex;
  justify-content: space-between;
  gap: var(--rr-space-4);
  align-items: flex-start;
}

.detail-summary {
  color: var(--rr-color-text-secondary);
  line-height: 1.6;
}

.detail-grid {
  grid-template-columns: repeat(auto-fit, minmax(280px, 1fr));
}

.detail-card {
  display: grid;
  gap: var(--rr-space-3);
  padding: var(--rr-space-4);
  border: 1px solid var(--rr-color-border-subtle);
  background: var(--rr-color-bg-surface-muted);
}

.detail-card ul {
  padding-left: 18px;
  color: var(--rr-color-text-secondary);
  display: grid;
  gap: var(--rr-space-2);
}

.relation-row {
  display: grid;
  gap: 6px;
  padding: var(--rr-space-4);
  background: rgb(255 255 255 / 0.92);
  border: 1px solid var(--rr-color-border-subtle);
}

@media (width <= 900px) {
  .panel-header,
  .detail-header {
    flex-direction: column;
  }
}
</style>
