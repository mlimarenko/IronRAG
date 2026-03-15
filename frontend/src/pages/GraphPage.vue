<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { RouterLink } from 'vue-router'

import { fetchProjects, fetchWorkspaces } from 'src/boot/api'
import PageSection from 'src/components/shell/PageSection.vue'
import StatusBadge from 'src/components/shell/StatusBadge.vue'
import EmptyStateCard from 'src/components/state/EmptyStateCard.vue'
import LoadingSkeletonPanel from 'src/components/state/LoadingSkeletonPanel.vue'
import { translateStatusLabel } from 'src/i18n/helpers'
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

const { t, tm } = useI18n()
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

function translateList(key: string): string[] {
  const value = tm(key)
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
      <RouterLink class="rr-button rr-button--secondary" to="/processing">
        {{ t('graph.actions.processing') }}
      </RouterLink>
      <RouterLink class="rr-button rr-button--secondary" to="/ingest">
        {{ t('graph.actions.ingest') }}
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
                  : currentCoverage?.relation_count
                    ? t('graph.panels.summary.blockerPartial')
                    : t('graph.panels.summary.blockerNoRows')
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
    </div>

    <article class="card workspace-panel detail-panel">
      <div class="panel-header">
        <div>
          <p class="rr-kicker">{{ t('graph.panels.detail.eyebrow') }}</p>
          <h3>{{ t('graph.panels.detail.title') }}</h3>
          <p class="panel-subtitle">{{ t('graph.panels.detail.description') }}</p>
        </div>
      </div>

      <LoadingSkeletonPanel
        v-if="loadingDetail"
        :title="t('graph.panels.detail.loading')"
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
