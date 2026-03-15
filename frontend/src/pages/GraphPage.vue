<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { RouterLink } from 'vue-router'

import { fetchProjects, fetchWorkspaces } from 'src/boot/api'
import PageSection from 'src/components/shell/PageSection.vue'
import CrossSurfaceGuide from 'src/components/shell/CrossSurfaceGuide.vue'
import ProductSpine from 'src/components/shell/ProductSpine.vue'
import StatusBadge from 'src/components/shell/StatusBadge.vue'
import EmptyStateCard from 'src/components/state/EmptyStateCard.vue'
import LoadingSkeletonPanel from 'src/components/state/LoadingSkeletonPanel.vue'
import { translateStatusLabel } from 'src/i18n/helpers'
import {
  fetchGraphEntityDetail,
  fetchGraphProductSnapshot,
  fetchGraphProjectDiagnostics,
  fetchGraphProjectSummary,
  fetchGraphSubgraph,
  isGraphApiUnavailableError,
  searchGraphProduct,
  type GraphEntityDetailResponse,
  type GraphEntitySummary,
  type GraphProductSnapshot,
  type GraphProjectDiagnosticsResponse,
  type GraphProjectSummaryResponse,
  type GraphRelationDetail,
  type GraphRelationSummary,
  type GraphSearchResponse,
  type GraphSubgraphResponse,
} from 'src/lib/graphProduct'
import {
  getSelectedProjectId,
  getSelectedWorkspaceId,
  setSelectedProjectId,
} from 'src/stores/flow'
import { setWorkspaceWithProjectReset, syncWorkspaceProjectScope } from 'src/lib/flowSelection'

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

const i18n = useI18n()
const { t } = i18n
const workspaces = ref<WorkspaceItem[]>([])
const projects = ref<ProjectItem[]>([])

const selectedWorkspaceId = ref(getSelectedWorkspaceId())
const selectedProjectId = ref(getSelectedProjectId())
const searchQuery = ref('')
const subgraphDepth = ref(1)

const productSnapshot = ref<GraphProductSnapshot | null>(null)
const projectSummary = ref<GraphProjectSummaryResponse | null>(null)
const projectDiagnostics = ref<GraphProjectDiagnosticsResponse | null>(null)
const searchResponse = ref<GraphSearchResponse | null>(null)
const entityDetail = ref<GraphEntityDetailResponse | null>(null)
const entitySubgraph = ref<GraphSubgraphResponse | null>(null)

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
const currentCoverage = computed(
  () => projectDiagnostics.value?.coverage ?? projectSummary.value?.coverage ?? productSnapshot.value?.coverage ?? null,
)
const coverageWarning = computed(
  () => projectDiagnostics.value?.coverage.warning ?? currentCoverage.value?.warning ?? null,
)
const readinessSummary = computed(() => projectDiagnostics.value?.readiness ?? null)
const contentSummary = computed(() => projectDiagnostics.value?.content ?? null)
const provenanceSummary = computed(() => projectDiagnostics.value?.provenance ?? null)
const diagnosticsBlockers = computed(() => readinessSummary.value?.blockers ?? [])
const diagnosticsNextSteps = computed(() => readinessSummary.value?.next_steps ?? [])
const showTechnicalDiagnostics = ref(false)
const showTechnicalDetail = ref(false)
const canLoadSubgraph = computed(() =>
  Boolean(selectedProjectId.value && selectedCard.value?.kind === 'entity' && !apiUnavailable.value),
)
const selectedSubgraphEntityName = computed(() =>
  selectedCard.value?.kind === 'entity' ? selectedCard.value.title : '',
)

const pageStatus = computed(() => {
  if (!selectedProject.value) {
    return { status: 'blocked', label: t('graph.states.chooseProject') }
  }

  if (loadingSurface.value) {
    return { status: 'pending', label: t('graph.states.loadingSurface') }
  }

  if (apiUnavailable.value) {
    return { status: 'blocked', label: t('graph.states.backendPending') }
  }

  if (surfaceError.value) {
    return { status: 'warning', label: t('graph.states.surfaceDegraded') }
  }

  return {
    status: currentCoverage.value?.status ?? 'draft',
    label: translateStatusLabel(currentCoverage.value?.status ?? 'preview'),
  }
})

const translateList = (key: string): string[] => {
  const value = i18n.tm(key)
  return Array.isArray(value) ? value.filter((item): item is string => typeof item === 'string') : []
}

const graphSummary = computed(() => {
  if (!selectedProject.value) {
    return {
      status: t('graph.surface.noProject.status'),
      headline: t('graph.surface.noProject.headline'),
      body: t('graph.surface.noProject.body'),
      highlights: translateList('graph.surface.noProject.highlights'),
    }
  }

  if (apiUnavailable.value) {
    return {
      status: t('graph.surface.unavailable.status'),
      headline: t('graph.surface.unavailable.headline'),
      body: t('graph.surface.unavailable.body'),
      highlights: translateList('graph.surface.unavailable.highlights'),
    }
  }

  if (
    currentCoverage.value &&
    (currentCoverage.value.entity_count > 0 || currentCoverage.value.relation_count > 0)
  ) {
    return {
      status: t('graph.surface.live.status'),
      headline: t('graph.surface.live.headline'),
      body: t('graph.surface.live.body'),
      highlights: translateList('graph.surface.live.highlights'),
    }
  }

  return {
    status: t('graph.surface.waiting.status'),
    headline: t('graph.surface.waiting.headline'),
    body: t('graph.surface.waiting.body'),
    highlights: translateList('graph.surface.waiting.highlights'),
  }
})

const productMetrics = computed<GraphProductMetric[]>(() => [
  {
    label: t('graph.metricLabels.entities'),
    value: currentCoverage.value
      ? formatCount(currentCoverage.value.entity_count, 'entity')
      : t('graph.metricLabels.noProjectSelected'),
    tone: currentCoverage.value && currentCoverage.value.entity_count > 0 ? 'good' : 'warning',
  },
  {
    label: t('graph.metricLabels.relations'),
    value: currentCoverage.value
      ? formatCount(currentCoverage.value.relation_count, 'relation')
      : t('graph.metricLabels.awaitingProjectScope'),
    tone: currentCoverage.value && currentCoverage.value.relation_count > 0 ? 'good' : 'warning',
  },
  {
    label: t('graph.metricLabels.extractionRuns'),
    value: currentCoverage.value
      ? formatCount(currentCoverage.value.extraction_runs, 'run')
      : apiUnavailable.value
        ? t('graph.metricLabels.backendRoutePending')
        : t('graph.metricLabels.awaitingProjectScope'),
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
    subtitle: 'Entity',
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
      entitySubgraph.value = null
      detailError.value = null
      return
    }

    const currentSelection = selectedItem.value

    if (currentSelection) {
      const { id, kind } = currentSelection

      if (items.some((item) => item.id === id && item.kind === kind)) {
        return
      }
    }

    selectedItem.value = {
      id: items[0].id,
      kind: items[0].kind,
    }
  },
  { immediate: true },
)

watch([selectedItem, subgraphDepth], ([item]) => {
  detailRequestId += 1
  entityDetail.value = null
  entitySubgraph.value = null
  detailError.value = null
  loadingDetail.value = false

  if (item?.kind !== 'entity' || !selectedProjectId.value || apiUnavailable.value) {
    return
  }

  const requestId = detailRequestId
  loadingDetail.value = true

  void Promise.all([
    fetchGraphEntityDetail(selectedProjectId.value, item.id),
    fetchGraphSubgraph(selectedProjectId.value, item.id, subgraphDepth.value),
  ])
    .then(([detail, subgraph]) => {
      if (requestId !== detailRequestId) {
        return
      }

      entityDetail.value = detail
      entitySubgraph.value = subgraph
    })
    .catch((error: unknown) => {
      if (requestId !== detailRequestId) {
        return
      }

      detailError.value =
        error instanceof Error ? error.message : t('graph.errors.loadEntityDetail')
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

        searchError.value = error instanceof Error ? error.message : t('graph.errors.searchFailed')
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
    surfaceError.value = error instanceof Error ? error.message : t('graph.errors.loadPageContext')
  }
})

onBeforeUnmount(() => {
  if (searchTimer) {
    window.clearTimeout(searchTimer)
  }
})

async function loadContext() {
  workspaces.value = await fetchWorkspaces()
  const scope = syncWorkspaceProjectScope(workspaces.value, [])
  selectedWorkspaceId.value = scope.workspaceId

  if (!selectedWorkspaceId.value) {
    projects.value = []
    selectedProjectId.value = ''
    setSelectedProjectId('')
    return
  }

  projects.value = await fetchProjects(selectedWorkspaceId.value)
  const refreshedScope = syncWorkspaceProjectScope(workspaces.value, projects.value)
  selectedWorkspaceId.value = refreshedScope.workspaceId
  selectedProjectId.value = refreshedScope.projectId
}

async function loadGraphSurface(projectId: string) {
  surfaceRequestId += 1
  const requestId = surfaceRequestId

  searchQuery.value = ''
  searchResponse.value = null
  entityDetail.value = null
  entitySubgraph.value = null
  selectedItem.value = null
  apiUnavailable.value = false
  surfaceError.value = null
  searchError.value = null
  detailError.value = null
  productSnapshot.value = null
  projectSummary.value = null
  projectDiagnostics.value = null
  subgraphDepth.value = 1

  if (!projectId) {
    loadingSurface.value = false
    return
  }

  loadingSurface.value = true

  try {
    const [snapshot, summary, diagnostics] = await Promise.all([
      fetchGraphProductSnapshot(projectId),
      fetchGraphProjectSummary(projectId),
      fetchGraphProjectDiagnostics(projectId),
    ])

    if (requestId !== surfaceRequestId) {
      return
    }

    productSnapshot.value = snapshot
    projectSummary.value = summary
    projectDiagnostics.value = diagnostics
  } catch (error) {
    if (requestId !== surfaceRequestId) {
      return
    }

    if (isGraphApiUnavailableError(error)) {
      apiUnavailable.value = true
      surfaceError.value = null
    } else {
      surfaceError.value = error instanceof Error ? error.message : t('graph.errors.loadCoverage')
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

function handleSubgraphDepthChange(event: Event) {
  const target = event.target
  if (!(target instanceof HTMLSelectElement)) {
    return
  }

  subgraphDepth.value = Number.parseInt(target.value, 10) || 1
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
    return t('graph.common.noGraphRows')
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

function formatRelationLine(relation: GraphRelationDetail): string {
  return `${relation.from_entity_name} ${relation.relation.relation_type} ${relation.to_entity_name}`
}
</script>

<template>
  <PageSection
    :eyebrow="t('graph.page.eyebrow')"
    :title="t('graph.page.title')"
    :description="t('graph.page.description')"
    :status="pageStatus.status"
    :status-label="pageStatus.label"
  >
    <template #actions>
      <RouterLink class="rr-button rr-button--secondary" to="/home">
        {{ t('graph.actions.processing') }}
      </RouterLink>
      <RouterLink class="rr-button rr-button--secondary" to="/files">
        {{ t('graph.actions.ingest') }}
      </RouterLink>
    </template>

    <ProductSpine active-section="graph" />
    <CrossSurfaceGuide active-section="graph" />

    <section class="hero card">
      <div class="hero__copy">
        <p class="hero__eyebrow rr-kicker">{{ graphSummary.status }}</p>
        <h2>{{ graphSummary.headline }}</h2>
        <p>{{ graphSummary.body }}</p>
        <p class="hero__note rr-note">{{ t('graph.page.technicalNote') }}</p>
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

    <div class="workspace-grid workspace-grid--triple">
      <article class="card workspace-panel">
        <div class="panel-header">
          <div>
            <p class="rr-kicker">{{ t('graph.panels.summary.eyebrow') }}</p>
            <h3>{{ t('graph.panels.summary.title') }}</h3>
            <p class="panel-subtitle">{{ t('graph.panels.summary.description') }}</p>
          </div>
          <StatusBadge :status="pageStatus.status" :label="pageStatus.label" />
        </div>

        <div class="summary-list">
          <article class="summary-row">
            <span class="summary-row__label">{{ t('graph.panels.summary.workspace') }}</span>
            <strong>{{ selectedWorkspace?.name ?? t('graph.panels.summary.workspaceEmpty') }}</strong>
          </article>
          <article class="summary-row">
            <span class="summary-row__label">{{ t('graph.panels.summary.project') }}</span>
            <div class="summary-row__control">
              <select
                class="rr-control"
                :value="selectedProjectId"
                :disabled="projects.length === 0"
                @change="handleProjectChange"
              >
                <option value="">{{ t('graph.panels.summary.projectPlaceholder') }}</option>
                <option v-for="project in projects" :key="project.id" :value="project.id">
                  {{ project.name }}
                </option>
              </select>
            </div>
          </article>
          <article class="summary-row">
            <span class="summary-row__label">{{ t('graph.panels.summary.relationKinds') }}</span>
            <strong>{{ relationKinds }}</strong>
          </article>
          <article class="summary-row">
            <span class="summary-row__label">{{ t('graph.panels.summary.entityKinds') }}</span>
            <strong>{{ entityKinds }}</strong>
          </article>
          <article class="summary-row">
            <span class="summary-row__label">{{ t('graph.panels.summary.currentBlocker') }}</span>
            <strong>
              {{
                apiUnavailable
                  ? t('graph.panels.summary.blockerApiUnavailable')
                  : readinessSummary?.blockers?.[0] ??
                    (currentCoverage?.relation_count
                      ? t('graph.panels.summary.blockerPartial')
                      : t('graph.panels.summary.blockerNoRows'))
              }}
            </strong>
          </article>
        </div>
      </article>

      <article class="card workspace-panel">
        <div class="panel-header panel-header--stacked">
          <div>
            <p class="rr-kicker">{{ t('graph.panels.search.eyebrow') }}</p>
            <h3>{{ t('graph.panels.search.title') }}</h3>
            <p class="panel-subtitle">{{ t('graph.panels.search.description') }}</p>
          </div>
          <label class="search-field">
            <span class="search-field__label">{{ t('graph.panels.search.label') }}</span>
            <input
              v-model="searchQuery"
              type="text"
              :disabled="!selectedProjectId || apiUnavailable"
              :placeholder="t('graph.panels.search.placeholder')"
            />
          </label>
        </div>

        <LoadingSkeletonPanel
          v-if="loadingSurface"
          :title="t('graph.panels.search.loading')"
          :lines="5"
        />

        <EmptyStateCard
          v-else-if="!selectedProjectId"
          :title="t('graph.panels.search.noProject.title')"
          :message="t('graph.panels.search.noProject.message')"
          :hint="t('graph.panels.search.noProject.hint')"
        />

        <EmptyStateCard
          v-else-if="apiUnavailable"
          :title="t('graph.panels.search.unavailable.title')"
          :message="t('graph.panels.search.unavailable.message')"
          :hint="t('graph.panels.search.unavailable.hint')"
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
          :title="searchQuery.trim() ? t('graph.panels.search.noMatches.title') : t('graph.panels.search.noRows.title')"
          :message="
            searchQuery.trim()
              ? t('graph.panels.search.noMatches.message')
              : t('graph.panels.search.noRows.message')
          "
          :hint="
            searchQuery.trim()
              ? t('graph.panels.search.noMatches.hint')
              : t('graph.panels.search.noRows.hint')
          "
        />

        <p v-if="loadingSearch" class="rr-note">{{ t('graph.panels.search.searching') }}</p>
        <p v-if="searchError" class="rr-banner" data-tone="danger">
          {{ searchError }}
        </p>
      </article>

      <article class="card workspace-panel diagnostics-panel">
        <div class="panel-header panel-header--stacked">
          <div>
            <p class="rr-kicker">{{ t('graph.panels.diagnostics.eyebrow') }}</p>
            <h3>{{ t('graph.panels.diagnostics.title') }}</h3>
            <p class="panel-subtitle">{{ t('graph.panels.diagnostics.description') }}</p>
          </div>
          <StatusBadge
            :status="readinessSummary?.status ? 'warning' : 'draft'"
            :label="readinessSummary?.status ?? t('graph.panels.diagnostics.pending')"
          />
        </div>

        <LoadingSkeletonPanel
          v-if="loadingSurface"
          :title="t('graph.panels.diagnostics.loading')"
          :lines="6"
        />

        <EmptyStateCard
          v-else-if="!selectedProjectId"
          :title="t('graph.panels.diagnostics.noProject.title')"
          :message="t('graph.panels.diagnostics.noProject.message')"
          :hint="t('graph.panels.diagnostics.noProject.hint')"
        />

        <EmptyStateCard
          v-else-if="apiUnavailable"
          :title="t('graph.panels.diagnostics.unavailable.title')"
          :message="t('graph.panels.diagnostics.unavailable.message')"
          :hint="t('graph.panels.diagnostics.unavailable.hint')"
        />

        <template v-else>
          <div class="diagnostics-block diagnostics-block--primary">
            <h4>{{ t('graph.panels.diagnostics.blockersTitle') }}</h4>
            <ul v-if="diagnosticsBlockers.length" class="bullet-list">
              <li v-for="blocker in diagnosticsBlockers" :key="blocker">{{ blocker }}</li>
            </ul>
            <p v-else class="rr-note">{{ t('graph.panels.diagnostics.noBlockers') }}</p>
          </div>

          <div class="diagnostics-block diagnostics-block--primary">
            <h4>{{ t('graph.panels.diagnostics.nextStepsTitle') }}</h4>
            <ul v-if="diagnosticsNextSteps.length" class="bullet-list">
              <li v-for="step in diagnosticsNextSteps" :key="step">{{ step }}</li>
            </ul>
            <p v-else class="rr-note">{{ t('graph.panels.diagnostics.noNextSteps') }}</p>
          </div>

          <details class="technical-details" :open="showTechnicalDiagnostics">
            <summary @click.prevent="showTechnicalDiagnostics = !showTechnicalDiagnostics">
              <span>{{ t('graph.panels.diagnostics.technicalSummary') }}</span>
              <small>{{ t('graph.panels.diagnostics.technicalHint') }}</small>
            </summary>

            <div class="diagnostics-grid">
              <article class="metric-card" data-tone="default">
                <span class="metric-card__label">{{ t('graph.panels.diagnostics.metrics.documents') }}</span>
                <strong>{{ contentSummary ? formatCount(contentSummary.persisted_document_count, 'document') : '—' }}</strong>
              </article>
              <article class="metric-card" data-tone="default">
                <span class="metric-card__label">{{ t('graph.panels.diagnostics.metrics.chunks') }}</span>
                <strong>{{ contentSummary ? formatCount(contentSummary.persisted_chunk_count, 'chunk') : '—' }}</strong>
              </article>
              <article class="metric-card" data-tone="default">
                <span class="metric-card__label">{{ t('graph.panels.diagnostics.metrics.embeddings') }}</span>
                <strong>{{ contentSummary ? formatCount(contentSummary.embedded_chunk_count, 'embedding') : '—' }}</strong>
              </article>
              <article class="metric-card" data-tone="default">
                <span class="metric-card__label">{{ t('graph.panels.diagnostics.metrics.retrievalRuns') }}</span>
                <strong>{{ contentSummary ? formatCount(contentSummary.retrieval_run_count, 'run') : '—' }}</strong>
              </article>
              <article class="metric-card" data-tone="default">
                <span class="metric-card__label">{{ t('graph.panels.diagnostics.metrics.entityRefs') }}</span>
                <strong>
                  {{ provenanceSummary ? formatCount(provenanceSummary.entities_with_chunk_refs, 'entity') : '—' }}
                </strong>
              </article>
              <article class="metric-card" data-tone="default">
                <span class="metric-card__label">{{ t('graph.panels.diagnostics.metrics.relationRefs') }}</span>
                <strong>
                  {{ provenanceSummary ? formatCount(provenanceSummary.relations_with_chunk_refs, 'relation') : '—' }}
                </strong>
              </article>
            </div>
          </details>
        </template>
      </article>
    </div>

    <article class="card workspace-panel detail-panel">
      <div class="panel-header panel-header--stacked">
        <div>
          <p class="rr-kicker">{{ t('graph.panels.detail.eyebrow') }}</p>
          <h3>{{ t('graph.panels.detail.title') }}</h3>
          <p class="panel-subtitle">{{ t('graph.panels.detail.description') }}</p>
        </div>
        <details class="technical-details detail-technical" :open="showTechnicalDetail">
          <summary @click.prevent="showTechnicalDetail = !showTechnicalDetail">
            <span>{{ t('graph.panels.detail.technicalSummary') }}</span>
            <small>{{ t('graph.panels.detail.technicalHint') }}</small>
          </summary>
          <label class="subgraph-depth-field">
            <span class="search-field__label">{{ t('graph.panels.detail.subgraphDepth') }}</span>
            <select
              class="rr-control"
              :value="String(subgraphDepth)"
              :disabled="!canLoadSubgraph"
              @change="handleSubgraphDepthChange"
            >
              <option value="1">1 hop</option>
              <option value="2">2 hops</option>
              <option value="3">3 hops</option>
            </select>
          </label>
        </details>
      </div>

      <LoadingSkeletonPanel
        v-if="loadingDetail"
        :title="t('graph.panels.detail.loading')"
        :lines="6"
      />

      <template v-else-if="selectedEntityCard && entityDetail">
        <div class="detail-grid">
          <article class="detail-card">
            <p class="rr-kicker">{{ selectedEntityCard.subtitle }}</p>
            <h4>{{ entityDetail.entity.canonical_name }}</h4>
            <p class="rr-note">
              {{ t('graph.panels.detail.entitySummary', { count: entityDetail.observed_relation_count }) }}
            </p>

            <div class="token-section">
              <span class="token-section__label">{{ t('graph.panels.detail.aliases') }}</span>
              <div v-if="entityDetail.aliases.length" class="token-list">
                <span v-for="alias in entityDetail.aliases" :key="alias" class="token-chip">{{ alias }}</span>
              </div>
              <p v-else class="rr-note">{{ t('graph.panels.detail.noAliases') }}</p>
            </div>

            <div class="token-section">
              <span class="token-section__label">{{ t('graph.panels.detail.documents') }}</span>
              <div v-if="entityDetail.source_document_ids.length" class="token-list">
                <span v-for="documentId in entityDetail.source_document_ids" :key="documentId" class="token-chip token-chip--mono">{{ documentId }}</span>
              </div>
              <p v-else class="rr-note">{{ t('graph.panels.detail.noDocuments') }}</p>
            </div>

            <div class="token-section">
              <span class="token-section__label">{{ t('graph.panels.detail.chunks') }}</span>
              <div v-if="entityDetail.source_chunk_ids.length" class="token-list">
                <span v-for="chunkId in entityDetail.source_chunk_ids" :key="chunkId" class="token-chip token-chip--mono">{{ chunkId }}</span>
              </div>
              <p v-else class="rr-note">{{ t('graph.panels.detail.noChunks') }}</p>
            </div>
          </article>

          <article class="detail-card">
            <div class="detail-card__header">
              <div>
                <p class="rr-kicker">{{ t('graph.panels.detail.subgraphEyebrow') }}</p>
                <h4>{{ t('graph.panels.detail.subgraphTitle', { name: selectedSubgraphEntityName || entityDetail.entity.canonical_name }) }}</h4>
              </div>
              <StatusBadge
                :label="t('graph.panels.detail.subgraphStats', { entities: entitySubgraph?.entity_count ?? 0, relations: entitySubgraph?.relation_count ?? 0 })"
              />
            </div>

            <details class="technical-details detail-technical" :open="showTechnicalDetail">
              <summary @click.prevent="showTechnicalDetail = !showTechnicalDetail">
                <span>{{ t('graph.panels.detail.subgraphSummary') }}</span>
                <small>{{ t('graph.panels.detail.subgraphHint', { depth: entitySubgraph?.depth ?? subgraphDepth }) }}</small>
              </summary>

              <div v-if="entitySubgraph?.entities.length" class="token-section">
                <span class="token-section__label">{{ t('graph.panels.detail.subgraphEntities') }}</span>
                <div class="token-list">
                  <span v-for="entity in entitySubgraph.entities" :key="entity.id" class="token-chip">
                    {{ entity.canonical_name }}
                  </span>
                </div>
              </div>
              <p v-else class="rr-note">{{ t('graph.panels.detail.noSubgraphEntities') }}</p>

              <div class="relation-columns">
                <div>
                  <span class="token-section__label">{{ t('graph.panels.detail.outgoingRelations') }}</span>
                  <ul v-if="entityDetail.outgoing_relations.length" class="bullet-list bullet-list--compact">
                    <li v-for="relation in entityDetail.outgoing_relations" :key="relation.relation.id">
                      {{ formatRelationLine(relation) }}
                    </li>
                  </ul>
                  <p v-else class="rr-note">{{ t('graph.panels.detail.noOutgoingRelations') }}</p>
                </div>
                <div>
                  <span class="token-section__label">{{ t('graph.panels.detail.incomingRelations') }}</span>
                  <ul v-if="entityDetail.incoming_relations.length" class="bullet-list bullet-list--compact">
                    <li v-for="relation in entityDetail.incoming_relations" :key="relation.relation.id">
                      {{ formatRelationLine(relation) }}
                    </li>
                  </ul>
                  <p v-else class="rr-note">{{ t('graph.panels.detail.noIncomingRelations') }}</p>
                </div>
              </div>

              <div class="token-section">
                <span class="token-section__label">{{ t('graph.panels.detail.subgraphRelations') }}</span>
                <ul v-if="entitySubgraph?.relations.length" class="bullet-list bullet-list--compact">
                  <li v-for="relation in entitySubgraph.relations" :key="relation.relation.id">
                    {{ formatRelationLine(relation) }}
                  </li>
                </ul>
                <p v-else class="rr-note">{{ t('graph.panels.detail.noSubgraphRelations') }}</p>
              </div>
            </details>
          </article>
        </div>

        <p v-if="entityDetail.warning" class="rr-banner" data-tone="warning">
          {{ entityDetail.warning }}
        </p>
        <p v-if="entitySubgraph?.warning" class="rr-banner" data-tone="warning">
          {{ entitySubgraph.warning }}
        </p>
      </template>

      <template v-else-if="selectedRelationCard">
        <div class="detail-grid detail-grid--single">
          <article class="detail-card">
            <p class="rr-kicker">{{ selectedRelationCard.subtitle }}</p>
            <h4>{{ selectedRelationCard.title }}</h4>
            <p>{{ selectedRelationCard.summary }}</p>
            <div class="token-section">
              <span class="token-section__label">{{ t('graph.panels.detail.matchReasons') }}</span>
              <div v-if="selectedRelationCard.matchReasons.length" class="token-list">
                <span v-for="reason in selectedRelationCard.matchReasons" :key="reason" class="token-chip">{{ reason }}</span>
              </div>
              <p v-else class="rr-note">{{ t('graph.panels.detail.noMatchReasons') }}</p>
            </div>
          </article>
        </div>
      </template>

      <EmptyStateCard
        v-else-if="detailError"
        :title="t('graph.panels.detail.loadErrorTitle')"
        :message="detailError"
        :hint="t('graph.panels.detail.loadErrorHint')"
      />

      <EmptyStateCard
        v-else
        :title="t('graph.panels.detail.emptySelection.title')"
        :message="t('graph.panels.detail.emptySelection.message')"
        :hint="t('graph.panels.detail.emptySelection.hint')"
      />
    </article>
  </PageSection>
</template>

<style scoped>
.workspace-grid {
  display: grid;
  gap: 1.5rem;
  grid-template-columns: repeat(2, minmax(0, 1fr));
}

.workspace-grid--triple {
  grid-template-columns: repeat(3, minmax(0, 1fr));
}

.workspace-panel,
.detail-panel,
.hero,
.detail-card,
.metric-card {
  display: flex;
  flex-direction: column;
  gap: 1rem;
}

.panel-header,
.panel-header--stacked,
.detail-card__header {
  display: flex;
  justify-content: space-between;
  gap: 1rem;
}

.panel-header--stacked {
  flex-direction: column;
  align-items: stretch;
}

.panel-subtitle,
.rr-note {
  color: var(--rr-text-muted);
}

.hero__note {
  max-width: 48rem;
}

.summary-list,
.search-results,
.detail-grid,
.relation-columns,
.diagnostics-grid {
  display: grid;
  gap: 1rem;
}

.technical-details {
  padding: 0.875rem 1rem;
  border: 1px dashed var(--rr-color-border-subtle, rgb(148 163 184 / 0.4));
  border-radius: var(--rr-radius-md, 16px);
  background: rgb(248 250 252 / 0.7);
}

.technical-details summary {
  display: grid;
  gap: 0.25rem;
  cursor: pointer;
  list-style: none;
}

.technical-details summary::-webkit-details-marker {
  display: none;
}

.technical-details[open] {
  gap: 1rem;
}

.diagnostics-block--primary {
  padding: 1rem;
  border: 1px solid rgb(15 23 42 / 0.06);
  border-radius: 1rem;
  background: rgb(255 255 255 / 0.72);
}

.detail-technical {
  display: grid;
  gap: 0.875rem;
}

.summary-row {
  display: grid;
  gap: 0.375rem;
}

.summary-row__label,
.metric-card__label,
.search-field__label,
.token-section__label,
.search-result__kind {
  font-size: 0.875rem;
  color: var(--rr-text-muted);
}

.summary-row__control,
.search-field,
.subgraph-depth-field,
.token-section,
.diagnostics-block {
  display: grid;
  gap: 0.5rem;
}

.search-results {
  align-content: start;
}

.search-result {
  display: grid;
  gap: 0.75rem;
  text-align: left;
  border: 1px solid var(--rr-border);
  border-radius: 1rem;
  padding: 1rem;
  background: var(--rr-surface);
}

.search-result:hover,
.search-result[data-active='true'] {
  border-color: var(--rr-accent);
  box-shadow: 0 0 0 1px color-mix(in srgb, var(--rr-accent) 35%, transparent);
}

.search-result__meta {
  display: grid;
  gap: 0.25rem;
}

.diagnostics-grid {
  grid-template-columns: repeat(2, minmax(0, 1fr));
}

.metric-card {
  border: 1px solid var(--rr-border);
  border-radius: 1rem;
  padding: 1rem;
  background: var(--rr-surface-muted);
}

.metric-card[data-tone='good'] {
  border-color: color-mix(in srgb, var(--rr-positive) 45%, var(--rr-border));
}

.metric-card[data-tone='warning'] {
  border-color: color-mix(in srgb, var(--rr-warning) 45%, var(--rr-border));
}

.detail-grid {
  grid-template-columns: repeat(2, minmax(0, 1fr));
}

.detail-grid--single {
  grid-template-columns: minmax(0, 1fr);
}

.detail-card {
  border: 1px solid var(--rr-border);
  border-radius: 1rem;
  padding: 1rem;
  background: var(--rr-surface-muted);
}

.token-list,
.bullet-list {
  display: flex;
  flex-wrap: wrap;
  gap: 0.5rem;
  margin: 0;
  padding: 0;
  list-style: none;
}

.bullet-list {
  display: grid;
  gap: 0.5rem;
  list-style: disc;
  padding-left: 1.25rem;
}

.bullet-list--compact {
  gap: 0.35rem;
}

.token-chip {
  border-radius: 999px;
  padding: 0.35rem 0.75rem;
  background: color-mix(in srgb, var(--rr-accent) 12%, white);
  border: 1px solid color-mix(in srgb, var(--rr-accent) 28%, var(--rr-border));
}

.token-chip--mono {
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, 'Liberation Mono', monospace;
  font-size: 0.8125rem;
}

@media (max-width: 1200px) {
  .workspace-grid,
  .workspace-grid--triple,
  .detail-grid,
  .diagnostics-grid,
  .relation-columns {
    grid-template-columns: minmax(0, 1fr);
  }
}
</style>
