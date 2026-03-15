<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { RouterLink } from 'vue-router'

import { fetchProjects, fetchWorkspaces } from 'src/boot/api'
import PageSection from 'src/components/shell/PageSection.vue'
import StatusBadge from 'src/components/shell/StatusBadge.vue'
import EmptyStateCard from 'src/components/state/EmptyStateCard.vue'
import LoadingSkeletonPanel from 'src/components/state/LoadingSkeletonPanel.vue'
import {
  fetchGraphEntityDetail,
  fetchGraphProductSnapshot,
  fetchGraphProjectSummary,
  isGraphApiUnavailableError,
  searchGraphProduct,
  type GraphEntityDetailResponse,
  type GraphEntitySummary,
  type GraphProductSnapshot,
  type GraphProjectSummaryResponse,
  type GraphRelationDetail,
  type GraphRelationSummary,
  type GraphSearchResponse,
} from 'src/lib/graphProduct'
import {
  getSelectedProjectId,
  getSelectedWorkspaceId,
  setSelectedProjectId,
  syncSelectedProjectId,
  syncSelectedWorkspaceId,
} from 'src/stores/flow'

interface WorkspaceItem {
  id: string
  slug: string
  name: string
}

interface ProjectItem {
  id: string
  slug: string
  name: string
  workspace_id: string
}

interface GraphProductMetric {
  label: string
  value: string
  tone?: 'default' | 'good' | 'warning'
}

interface GraphResultCard {
  id: string
  kind: 'entity' | 'relation'
  title: string
  subtitle: string
  summary: string
  badge: string
  sourceChunkCount: number
  matchReasons: string[]
  entity?: GraphEntitySummary
  relation?: GraphRelationSummary
  fromEntityName?: string
  toEntityName?: string
}

interface GraphSelection {
  id: string
  kind: 'entity' | 'relation'
}

const workspaces = ref<WorkspaceItem[]>([])
const projects = ref<ProjectItem[]>([])

const selectedWorkspaceId = ref(getSelectedWorkspaceId())
const selectedProjectId = ref(getSelectedProjectId())
const searchQuery = ref('')

const productSnapshot = ref<GraphProductSnapshot | null>(null)
const projectSummary = ref<GraphProjectSummaryResponse | null>(null)
const searchResponse = ref<GraphSearchResponse | null>(null)
const entityDetail = ref<GraphEntityDetailResponse | null>(null)

const loadingSurface = ref(false)
const loadingSearch = ref(false)
const loadingDetail = ref(false)
const apiUnavailable = ref(false)
const surfaceError = ref<string | null>(null)
const searchError = ref<string | null>(null)
const detailError = ref<string | null>(null)
const selectedItem = ref<GraphSelection | null>(null)

let searchTimer: number | undefined
let surfaceRequestId = 0
let searchRequestId = 0
let detailRequestId = 0

const selectedWorkspace = computed(
  () => workspaces.value.find((item) => item.id === selectedWorkspaceId.value) ?? null,
)
const selectedProject = computed(
  () => projects.value.find((item) => item.id === selectedProjectId.value) ?? null,
)
const entityNameById = computed<Record<string, string>>(() => {
  if (!productSnapshot.value) {
    return {}
  }

  return Object.fromEntries(
    productSnapshot.value.entities.map((entity) => [entity.id, entity.canonical_name]),
  )
})
const currentCoverage = computed(() => projectSummary.value?.coverage ?? productSnapshot.value?.coverage ?? null)
const coverageWarning = computed(() => currentCoverage.value?.warning ?? null)

const pageStatus = computed(() => {
  if (!selectedProject.value) {
    return { status: 'blocked', label: 'Choose project' }
  }

  if (loadingSurface.value) {
    return { status: 'pending', label: 'Loading graph surface' }
  }

  if (apiUnavailable.value) {
    return { status: 'blocked', label: 'Backend entry point pending' }
  }

  if (surfaceError.value) {
    return { status: 'warning', label: 'Graph surface degraded' }
  }

  return {
    status: currentCoverage.value?.status ?? 'draft',
    label: formatStatusLabel(currentCoverage.value?.status ?? 'preview'),
  }
})

const graphSummary = computed(() => {
  if (!selectedProject.value) {
    return {
      status: 'Blocked',
      headline: 'Select a project to inspect graph relations.',
      body: 'This screen is ready to show persisted entities and relation coverage as soon as a project scope is selected.',
      highlights: [
        'Project scope comes from the same workspace flow used by Ingest and Ask.',
        'The page stays explicit about missing context instead of inventing graph data.',
        'Once a project is selected, the screen probes live graph endpoints immediately.',
      ],
    }
  }

  if (apiUnavailable.value) {
    return {
      status: 'Entry point ready',
      headline: 'Graph UI is wired, but this backend build does not expose graph runtime routes yet.',
      body: 'The product surface is now project-scoped and ready for real graph data, but `/graph-products/*` still needs backend wiring in the running environment.',
      highlights: [
        'No fake entities or relations are rendered when the route is unavailable.',
        'Project selection, status mapping, and empty states are already product-ready.',
        'The same screen will light up automatically once graph routes ship on the backend.',
      ],
    }
  }

  if (
    currentCoverage.value &&
    (currentCoverage.value.entity_count > 0 || currentCoverage.value.relation_count > 0)
  ) {
    return {
      status: 'Live graph rows',
      headline: 'Inspect persisted entities, relation coverage, and search results for the selected project.',
      body: 'This view is reading real graph rows. Relation search and entity detail are live where the backend has persisted records.',
      highlights: [
        'Search results come from persisted entities and relation rows, not placeholder text.',
        'Entity detail exposes aliases, supporting documents, chunk references, and observed relations.',
        'Warnings stay visible when extraction tracking or provenance depth are still partial.',
      ],
    }
  }

  return {
    status: 'Waiting for extraction',
    headline: 'Graph endpoints respond, but this project has no persisted relation rows yet.',
    body: 'The screen is live against the backend, and the current blocker is runtime extraction populating `entity` and `relation` rows for this project.',
    highlights: [
      'The page confirms backend reachability even when graph counts are zero.',
      'Entity and relation counts stay at zero until extraction writes persisted rows.',
      'As soon as rows appear, search and detail panels switch to live data without UI changes.',
    ],
  }
})

const productMetrics = computed<GraphProductMetric[]>(() => [
  {
    label: 'Entities',
    value: currentCoverage.value
      ? formatCount(currentCoverage.value.entity_count, 'entity')
      : 'No project selected',
    tone: currentCoverage.value && currentCoverage.value.entity_count > 0 ? 'good' : 'warning',
  },
  {
    label: 'Relations',
    value: currentCoverage.value
      ? formatCount(currentCoverage.value.relation_count, 'relation')
      : 'Awaiting project scope',
    tone: currentCoverage.value && currentCoverage.value.relation_count > 0 ? 'good' : 'warning',
  },
  {
    label: 'Extraction runs',
    value: currentCoverage.value
      ? formatCount(currentCoverage.value.extraction_runs, 'run')
      : apiUnavailable.value
        ? 'Backend route pending'
        : 'Awaiting project scope',
    tone: currentCoverage.value && currentCoverage.value.extraction_runs > 0 ? 'good' : 'warning',
  },
])

const relationKinds = computed(() => summarizeKinds(projectSummary.value?.relation_kinds ?? []))
const entityKinds = computed(() => summarizeKinds(projectSummary.value?.entity_kinds ?? []))

const defaultResultCards = computed<GraphResultCard[]>(() => {
  if (!projectSummary.value) {
    return []
  }

  const entityCards = projectSummary.value.top_entities.map((entity) => ({
    id: entity.id,
    kind: 'entity' as const,
    title: entity.canonical_name,
    subtitle: entity.entity_type ?? 'Entity',
    summary: `${formatCount(entity.source_chunk_count, 'supporting chunk')} linked to this entity.`,
    badge: 'Entity',
    sourceChunkCount: entity.source_chunk_count,
    matchReasons: [],
    entity,
  }))
  const relationCards = projectSummary.value.sample_relations.map((relation) =>
    relationToCard(relation, entityNameById.value, []),
  )

  return [...entityCards, ...relationCards]
})

const searchResultCards = computed<GraphResultCard[]>(() => {
  if (!searchResponse.value) {
    return []
  }

  const entityCards = searchResponse.value.entity_results.map(({ entity, match_reasons }) => ({
    id: entity.id,
    kind: 'entity' as const,
    title: entity.canonical_name,
    subtitle: entity.entity_type ?? 'Entity',
    summary: formatReasonsSummary(match_reasons, entity.source_chunk_count),
    badge: 'Entity',
    sourceChunkCount: entity.source_chunk_count,
    matchReasons: match_reasons,
    entity,
  }))
  const relationCards = searchResponse.value.relation_results.map(
    ({ relation, from_entity_name, to_entity_name, match_reasons }) =>
      relationToCard(
        relation,
        entityNameById.value,
        match_reasons,
        from_entity_name,
        to_entity_name,
      ),
  )

  return [...entityCards, ...relationCards]
})

const visibleResults = computed<GraphResultCard[]>(() =>
  searchQuery.value.trim() ? searchResultCards.value : defaultResultCards.value,
)
const selectedCard = computed<GraphResultCard | null>(() => {
  if (!selectedItem.value) {
    return visibleResults.value[0] ?? null
  }

  const currentSelection = selectedItem.value

  return (
    visibleResults.value.find(
      (item) => item.kind === currentSelection.kind && item.id === currentSelection.id,
    ) ?? null
  )
})
const selectedEntityCard = computed(() =>
  selectedCard.value?.kind === 'entity' ? selectedCard.value : null,
)
const selectedRelationCard = computed(() =>
  selectedCard.value?.kind === 'relation' ? selectedCard.value : null,
)

watch(
  visibleResults,
  (items) => {
    if (!items.length) {
      selectedItem.value = null
      entityDetail.value = null
      detailError.value = null
      return
    }

  if (
    selectedItem.value &&
    items.some((item) => item.id === selectedItem.value.id && item.kind === selectedItem.value.kind)
  ) {
    return
  }

    selectedItem.value = {
      id: items[0].id,
      kind: items[0].kind,
    }
  },
  { immediate: true },
)

watch(selectedItem, (item) => {
  detailRequestId += 1
  entityDetail.value = null
  detailError.value = null
  loadingDetail.value = false

  if (item?.kind !== 'entity' || !selectedProjectId.value || apiUnavailable.value) {
    return
  }

  const requestId = detailRequestId
  loadingDetail.value = true

  void fetchGraphEntityDetail(selectedProjectId.value, item.id)
    .then((response) => {
      if (requestId !== detailRequestId) {
        return
      }

      entityDetail.value = response
    })
    .catch((error: unknown) => {
      if (requestId !== detailRequestId) {
        return
      }

      detailError.value = error instanceof Error ? error.message : 'Failed to load entity detail'
    })
    .finally(() => {
      if (requestId === detailRequestId) {
        loadingDetail.value = false
      }
    })
})

watch(searchQuery, (value) => {
  searchError.value = null
  searchResponse.value = null

  if (searchTimer) {
    window.clearTimeout(searchTimer)
  }

  const query = value.trim()
  if (!query || !selectedProjectId.value || apiUnavailable.value) {
    loadingSearch.value = false
    return
  }

  const requestId = ++searchRequestId
  loadingSearch.value = true

  searchTimer = window.setTimeout(() => {
    void searchGraphProduct(selectedProjectId.value, query)
      .then((response) => {
        if (requestId !== searchRequestId) {
          return
        }

        searchResponse.value = response
      })
      .catch((error: unknown) => {
        if (requestId !== searchRequestId) {
          return
        }

        searchError.value = error instanceof Error ? error.message : 'Graph search failed'
      })
      .finally(() => {
        if (requestId === searchRequestId) {
          loadingSearch.value = false
        }
      })
  }, 250)
})

onMounted(async () => {
  try {
    await loadContext()
    await loadGraphSurface(selectedProjectId.value)
  } catch (error) {
    surfaceError.value =
      error instanceof Error ? error.message : 'Failed to load graph page context'
  }
})

onBeforeUnmount(() => {
  if (searchTimer) {
    window.clearTimeout(searchTimer)
  }
})

async function loadContext() {
  workspaces.value = await fetchWorkspaces()
  selectedWorkspaceId.value = syncSelectedWorkspaceId(workspaces.value)

  if (!selectedWorkspaceId.value) {
    projects.value = []
    selectedProjectId.value = ''
    syncSelectedProjectId([])
    return
  }

  projects.value = await fetchProjects(selectedWorkspaceId.value)
  selectedProjectId.value = syncSelectedProjectId(projects.value)
}

async function loadGraphSurface(projectId: string) {
  surfaceRequestId += 1
  const requestId = surfaceRequestId

  searchQuery.value = ''
  searchResponse.value = null
  entityDetail.value = null
  selectedItem.value = null
  apiUnavailable.value = false
  surfaceError.value = null
  searchError.value = null
  detailError.value = null
  productSnapshot.value = null
  projectSummary.value = null

  if (!projectId) {
    loadingSurface.value = false
    return
  }

  loadingSurface.value = true

  try {
    const [snapshot, summary] = await Promise.all([
      fetchGraphProductSnapshot(projectId),
      fetchGraphProjectSummary(projectId),
    ])

    if (requestId !== surfaceRequestId) {
      return
    }

    productSnapshot.value = snapshot
    projectSummary.value = summary
  } catch (error) {
    if (requestId !== surfaceRequestId) {
      return
    }

    if (isGraphApiUnavailableError(error)) {
      apiUnavailable.value = true
      surfaceError.value = null
    } else {
      surfaceError.value =
        error instanceof Error ? error.message : 'Failed to load graph coverage'
    }
  } finally {
    if (requestId === surfaceRequestId) {
      loadingSurface.value = false
    }
  }
}

async function handleProjectSelection(projectId: string) {
  selectedProjectId.value = projectId
  setSelectedProjectId(projectId)
  await loadGraphSurface(projectId)
}

async function handleProjectChange(event: Event) {
  const target = event.target
  if (!(target instanceof HTMLSelectElement)) {
    return
  }

  await handleProjectSelection(target.value)
}

function selectCard(item: GraphResultCard) {
  selectedItem.value = {
    id: item.id,
    kind: item.kind,
  }
}

function relationToCard(
  relation: GraphRelationSummary,
  entityNames: Partial<Record<string, string>>,
  matchReasons: string[],
  fromEntityName?: string,
  toEntityName?: string,
): GraphResultCard {
  const fromName = fromEntityName ?? entityNames[relation.from_entity_id] ?? relation.from_entity_id
  const toName = toEntityName ?? entityNames[relation.to_entity_id] ?? relation.to_entity_id

  return {
    id: relation.id,
    kind: 'relation',
    title: `${fromName} ${relation.relation_type} ${toName}`,
    subtitle: 'Relation',
    summary: formatReasonsSummary(matchReasons, relation.source_chunk_count),
    badge: relation.relation_type,
    sourceChunkCount: relation.source_chunk_count,
    matchReasons,
    relation,
    fromEntityName: fromName,
    toEntityName: toName,
  }
}

function summarizeKinds(items: { name: string; count: number }[]): string {
  if (!items.length) {
    return 'No graph rows yet'
  }

  return items
    .slice(0, 3)
    .map((item) => `${item.name} (${String(item.count)})`)
    .join(', ')
}

function formatCount(value: number, singular: string): string {
  const plural = singular.endsWith('s') ? singular : `${singular}s`
  return `${String(value)} ${value === 1 ? singular : plural}`
}

function formatReasonsSummary(reasons: string[], chunkCount: number): string {
  if (!reasons.length) {
    return `${formatCount(chunkCount, 'supporting chunk')} linked to this record.`
  }

  return `Matched on ${reasons.join(', ')}. ${formatCount(chunkCount, 'supporting chunk')} linked to this record.`
}

function formatStatusLabel(value: string): string {
  return value
    .replace(/[_-]+/g, ' ')
    .replace(/\b\w/g, (char) => char.toUpperCase())
}

function formatRelationLine(relation: GraphRelationDetail): string {
  return `${relation.from_entity_name} ${relation.relation.relation_type} ${relation.to_entity_name}`
}
</script>

<template>
  <PageSection
    eyebrow="Knowledge graph"
    title="Graph"
    description="Inspect live graph coverage for the selected project, search persisted entities and relations when available, and keep backend blockers explicit when runtime extraction is still missing."
    :status="pageStatus.status"
    :status-label="pageStatus.label"
  >
    <template #actions>
      <RouterLink class="rr-button rr-button--secondary" to="/setup">
        Setup scope
      </RouterLink>
      <RouterLink class="rr-button rr-button--secondary" to="/ingest">
        Ingest content
      </RouterLink>
    </template>

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

      <p v-if="coverageWarning" class="rr-banner" data-tone="warning">
        {{ coverageWarning }}
      </p>
      <p v-if="surfaceError" class="rr-banner" data-tone="danger">
        {{ surfaceError }}
      </p>
    </section>

    <div class="workspace-grid">
      <article class="card workspace-panel">
        <div class="panel-header">
          <div>
            <p class="rr-kicker">Scope and readiness</p>
            <h3>Graph summary</h3>
            <p class="panel-subtitle">
              Project-scoped graph readiness, live coverage, and the blocker that still keeps
              relation extraction partial.
            </p>
          </div>
          <StatusBadge :status="pageStatus.status" :label="pageStatus.label" />
        </div>

        <div class="summary-list">
          <article class="summary-row">
            <span class="summary-row__label">Workspace</span>
            <strong>{{ selectedWorkspace?.name ?? 'No workspace selected' }}</strong>
          </article>
          <article class="summary-row">
            <span class="summary-row__label">Project</span>
            <div class="summary-row__control">
              <select
                class="rr-control"
                :value="selectedProjectId"
                :disabled="projects.length === 0"
                @change="handleProjectChange"
              >
                <option value="">Select a project</option>
                <option v-for="project in projects" :key="project.id" :value="project.id">
                  {{ project.name }}
                </option>
              </select>
            </div>
          </article>
          <article class="summary-row">
            <span class="summary-row__label">Relation kinds</span>
            <strong>{{ relationKinds }}</strong>
          </article>
          <article class="summary-row">
            <span class="summary-row__label">Entity kinds</span>
            <strong>{{ entityKinds }}</strong>
          </article>
          <article class="summary-row">
            <span class="summary-row__label">Current blocker</span>
            <strong>
              {{
                apiUnavailable
                  ? 'Backend route is not wired in this runtime build yet.'
                  : currentCoverage?.relation_count
                    ? 'Extraction tracking and provenance depth remain partial.'
                    : 'Runtime extraction has not written entity/relation rows for this project yet.'
              }}
            </strong>
          </article>
        </div>
      </article>

      <article class="card workspace-panel">
        <div class="panel-header panel-header--stacked">
          <div>
            <p class="rr-kicker">Discovery</p>
            <h3>Graph search</h3>
            <p class="panel-subtitle">
              Search persisted entities and relations when the graph runtime is available. Without a
              query, the panel shows top entities and sample relations.
            </p>
          </div>
          <label class="search-field">
            <span class="search-field__label">Search graph concepts</span>
            <input
              v-model="searchQuery"
              type="text"
              :disabled="!selectedProjectId || apiUnavailable"
              placeholder="Search entities, relations, aliases..."
            />
          </label>
        </div>

        <LoadingSkeletonPanel
          v-if="loadingSurface"
          title="Loading graph"
          :lines="5"
        />

        <EmptyStateCard
          v-else-if="!selectedProjectId"
          title="Select a project first"
          message="Graph is scoped per project. Choose a project to inspect entity and relation coverage."
          hint="The selector in this panel uses the same session scope as the rest of the operator shell."
        />

        <EmptyStateCard
          v-else-if="apiUnavailable"
          title="Graph backend route is not available"
          message="This product surface is ready, but the running backend does not expose `/graph-products/*` yet."
          hint="Backend wiring is the remaining blocker before live entity and relation data can appear here."
        />

        <div v-else-if="visibleResults.length" class="search-results">
          <button
            v-for="item in visibleResults"
            :key="`${item.kind}-${item.id}`"
            type="button"
            class="search-result"
            :data-active="selectedCard?.id === item.id && selectedCard?.kind === item.kind"
            @click="selectCard(item)"
          >
            <div class="search-result__meta">
              <span class="search-result__kind">{{ item.subtitle }}</span>
              <strong>{{ item.title }}</strong>
            </div>
            <StatusBadge :label="item.badge" />
            <p>{{ item.summary }}</p>
          </button>
        </div>

        <EmptyStateCard
          v-else
          :title="searchQuery.trim() ? 'No graph matches yet' : 'No graph rows yet'"
          :message="
            searchQuery.trim()
              ? 'No persisted entities or relations matched that search.'
              : 'This project does not have persisted graph rows yet.'
          "
          :hint="
            searchQuery.trim()
              ? 'Try broader terms like a canonical entity name, alias, or relation type.'
              : 'Once extraction writes entity and relation rows, the search panel will populate automatically.'
          "
        />

        <p v-if="loadingSearch" class="rr-note">Searching graph records...</p>
        <p v-if="searchError" class="rr-banner" data-tone="danger">
          {{ searchError }}
        </p>
      </article>
    </div>

    <article class="card workspace-panel detail-panel">
      <div class="panel-header">
        <div>
          <p class="rr-kicker">Detail</p>
          <h3>Graph detail</h3>
          <p class="panel-subtitle">
            Inspect the selected entity or relation without inventing provenance the backend does
            not actually expose yet.
          </p>
        </div>
      </div>

      <LoadingSkeletonPanel
        v-if="loadingDetail"
        title="Loading detail"
        :lines="4"
      />

      <template v-else-if="selectedEntityCard && entityDetail">
        <div class="detail-header">
          <div>
            <p class="detail-header__kind">{{ selectedEntityCard.subtitle }}</p>
            <h4>{{ selectedEntityCard.title }}</h4>
          </div>
          <StatusBadge status="Ready" label="Live entity detail" />
        </div>

        <p class="detail-summary">
          Entity detail is coming from persisted graph rows. Aliases, source documents, source
          chunks, and observed incoming/outgoing relations are live for this record.
        </p>

        <p v-if="entityDetail.warning" class="rr-banner" data-tone="warning">
          {{ entityDetail.warning }}
        </p>

        <div class="detail-grid">
          <section class="detail-card">
            <h5>Entity evidence</h5>
            <ul>
              <li><strong>Aliases:</strong> {{ entityDetail.aliases.join(', ') || 'None recorded' }}</li>
              <li>
                <strong>Source documents:</strong>
                {{ formatCount(entityDetail.source_document_ids.length, 'document') }}
              </li>
              <li>
                <strong>Source chunks:</strong>
                {{ formatCount(entityDetail.source_chunk_ids.length, 'chunk') }}
              </li>
              <li>
                <strong>Observed relations:</strong>
                {{ formatCount(entityDetail.observed_relation_count, 'relation') }}
              </li>
            </ul>
          </section>

          <section class="detail-card">
            <h5>Outgoing relations</h5>
            <div v-if="entityDetail.outgoing_relations.length" class="relation-list">
              <article
                v-for="relation in entityDetail.outgoing_relations"
                :key="relation.relation.id"
                class="relation-row"
              >
                <strong>{{ formatRelationLine(relation) }}</strong>
                <span>{{ formatCount(relation.relation.source_chunk_count, 'chunk') }}</span>
              </article>
            </div>
            <EmptyStateCard
              v-else
              title="No outgoing relations"
              message="This entity currently has no outgoing relations in persisted graph rows."
            />
          </section>

          <section class="detail-card">
            <h5>Incoming relations</h5>
            <div v-if="entityDetail.incoming_relations.length" class="relation-list">
              <article
                v-for="relation in entityDetail.incoming_relations"
                :key="relation.relation.id"
                class="relation-row"
              >
                <strong>{{ formatRelationLine(relation) }}</strong>
                <span>{{ formatCount(relation.relation.source_chunk_count, 'chunk') }}</span>
              </article>
            </div>
            <EmptyStateCard
              v-else
              title="No incoming relations"
              message="This entity currently has no incoming relations in persisted graph rows."
            />
          </section>
        </div>
      </template>

      <template v-else-if="selectedRelationCard">
        <div class="detail-header">
          <div>
            <p class="detail-header__kind">{{ selectedRelationCard.subtitle }}</p>
            <h4>{{ selectedRelationCard.title }}</h4>
          </div>
          <StatusBadge status="Partial" label="Relation summary only" />
        </div>

        <p class="detail-summary">
          Relation coverage is live enough to show the tuple and supporting chunk count. A dedicated
          relation detail endpoint with richer provenance is still a backend follow-up.
        </p>

        <div class="detail-grid">
          <section class="detail-card">
            <h5>Relation tuple</h5>
            <ul>
              <li><strong>From:</strong> {{ selectedRelationCard.fromEntityName }}</li>
              <li>
                <strong>Relation type:</strong>
                {{ selectedRelationCard.relation?.relation_type ?? selectedRelationCard.badge }}
              </li>
              <li><strong>To:</strong> {{ selectedRelationCard.toEntityName }}</li>
            </ul>
          </section>

          <section class="detail-card">
            <h5>Current evidence</h5>
            <ul>
              <li>
                <strong>Supporting chunks:</strong>
                {{ formatCount(selectedRelationCard.sourceChunkCount, 'chunk') }}
              </li>
              <li>
                <strong>Matched fields:</strong>
                {{ selectedRelationCard.matchReasons.join(', ') || 'Top relation coverage sample' }}
              </li>
              <li><strong>Status:</strong> Relation tuple is visible; deep provenance is still partial.</li>
            </ul>
          </section>
        </div>
      </template>

      <EmptyStateCard
        v-else-if="detailError"
        title="Entity detail could not be loaded"
        :message="detailError"
        hint="Coverage and search results can still be reviewed while backend detail for this entity is investigated."
      />

      <EmptyStateCard
        v-else
        title="No graph detail selected"
        message="Pick an entity or relation from the search panel to inspect live graph coverage."
        hint="The detail panel only renders persisted graph data and explicit blockers."
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

.summary-row__control {
  width: 100%;
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

.search-result :deep(.status-badge) {
  width: fit-content;
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
  display: grid;
  gap: 10px;
  padding-left: 18px;
}

.relation-row {
  display: flex;
  justify-content: space-between;
  gap: var(--rr-space-3);
  align-items: center;
  padding: var(--rr-space-3) var(--rr-space-4);
  background: var(--rr-color-bg-surface-muted);
}

.relation-row strong {
  flex: 1;
}

.relation-row span {
  color: var(--rr-color-text-muted);
  white-space: nowrap;
}

@media (width <= 720px) {
  .card {
    padding: var(--rr-space-5);
  }

  .panel-header,
  .detail-header,
  .relation-row {
    flex-direction: column;
    align-items: flex-start;
  }
}
</style>
