<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import SearchField from 'src/components/design-system/SearchField.vue'
import { useDisplayFormatters } from 'src/composables/useDisplayFormatters'
import type { AdminApiTokenRow } from 'src/models/ui/admin'

const props = defineProps<{
  rows: AdminApiTokenRow[]
  currentPrincipalId: string | null
  currentPrincipalLabel: string | null
  workspaceName: string
  libraryName: string
  loading?: boolean
  errorMessage?: string | null
}>()

const emit = defineEmits<{
  create: []
  copy: [principalId: string]
  revoke: [principalId: string]
}>()
const { t } = useI18n()
const { enumLabel, formatDateTime, permissionLabel, statusBadgeLabel } = useDisplayFormatters()
const searchQuery = ref('')
const selectedPrincipalId = ref<string | null>(null)

const filteredRows = computed(() => {
  const query = searchQuery.value.trim().toLowerCase()
  if (!query) {
    return props.rows
  }
  return props.rows.filter((row) => {
    const haystack = [
      row.label,
      row.tokenPrefix,
      row.status,
      principalLabel(row),
      grantsSummary(row),
      scopeSummary(row),
      ...row.grants.map((grant) => permissionLabel(grant.permissionKind)),
      ...row.grants.map((grant) => enumLabel('admin.tokens.resourceKinds', grant.resourceKind)),
    ]
      .join(' ')
      .toLowerCase()
    return haystack.includes(query)
  })
})

const selectedRow = computed(
  () => filteredRows.value.find((row) => row.principalId === selectedPrincipalId.value) ?? null,
)

const summary = computed(() => {
  const now = Date.now()
  const soon = now + 7 * 24 * 60 * 60 * 1000
  return {
    total: props.rows.length,
    active: props.rows.filter((row) => row.status === 'active').length,
    expiringSoon: props.rows.filter((row) => {
      if (!row.expiresAt || row.status !== 'active') {
        return false
      }
      const expiresAt = new Date(row.expiresAt).getTime()
      return Number.isFinite(expiresAt) && expiresAt <= soon
    }).length,
  }
})
const showSummary = computed(() => {
  if (summary.value.total === 0) {
    return false
  }
  if (summary.value.expiringSoon > 0) {
    return true
  }
  return summary.value.total > 1
})

const showLoadingState = computed(() => Boolean(props.loading) && props.rows.length === 0)
const showEmptyState = computed(() => !showLoadingState.value && props.rows.length === 0)
const showNoResultsState = computed(
  () => !showLoadingState.value && props.rows.length > 0 && filteredRows.value.length === 0,
)
const showSparseWorkbench = computed(() => showEmptyState.value || showNoResultsState.value)
const showSingleTokenWorkbench = computed(
  () => !showSparseWorkbench.value && filteredRows.value.length === 1,
)
const genericPrincipalKindLabel = computed(() =>
  enumLabel('admin.tokens.principalKinds', 'api_token'),
)

watch(
  filteredRows,
  (rows) => {
    if (rows.length === 0) {
      selectedPrincipalId.value = null
      return
    }
    if (!rows.some((row) => row.principalId === selectedPrincipalId.value)) {
      selectedPrincipalId.value = rows[0].principalId
    }
  },
  { immediate: true },
)

function principalLabel(row: AdminApiTokenRow): string {
  if (props.currentPrincipalId && row.principalId === props.currentPrincipalId) {
    return props.currentPrincipalLabel?.trim() || row.label
  }
  return enumLabel('admin.tokens.principalKinds', 'api_token')
}

function activitySummary(value: string | null): string {
  return value ? formatDateTime(value) : '—'
}

function statusClass(status: string): string {
  if (status === 'active') {
    return 'is-success'
  }
  if (status === 'revoked') {
    return 'is-danger'
  }
  return 'is-muted'
}

function grantsSummary(row: AdminApiTokenRow): string {
  if (row.grants.length === 0) {
    return '—'
  }
  if (row.grants.length === 1) {
    const grant = row.grants[0]
    return `${permissionLabel(grant.permissionKind)} · ${enumLabel('admin.tokens.resourceKinds', grant.resourceKind)}`
  }
  return t('admin.tokens.grantsSummary', { count: row.grants.length })
}

function scopeSummary(row: AdminApiTokenRow): string {
  const kinds = Array.from(new Set(row.grants.map((grant) => grant.resourceKind)))
  if (kinds.length === 0) {
    return '—'
  }
  if (kinds.includes('library')) {
    return t('admin.tokens.scope.library', { library: props.libraryName })
  }
  if (kinds.includes('workspace')) {
    return t('admin.tokens.scope.workspace', { workspace: props.workspaceName })
  }
  if (kinds.length === 1) {
    return enumLabel('admin.tokens.resourceKinds', kinds[0])
  }
  return t('admin.tokens.multiScope', { count: kinds.length })
}

function secondaryMeta(row: AdminApiTokenRow): string {
  if (row.revokedAt) {
    return `${t('admin.tokens.revokedAt')}: ${activitySummary(row.revokedAt)}`
  }
  if (row.expiresAt) {
    return `${t('admin.headers.expires')}: ${activitySummary(row.expiresAt)}`
  }
  return `${t('admin.headers.lastUsed')}: ${activitySummary(row.lastUsedAt)}`
}

function showPrincipalFact(row: AdminApiTokenRow): boolean {
  return principalLabel(row) !== genericPrincipalKindLabel.value
}

function selectRow(principalId: string): void {
  selectedPrincipalId.value = principalId
}

function clearSearch(): void {
  searchQuery.value = ''
}
</script>

<template>
  <section
    class="rr-admin-workbench rr-admin-workbench--access"
    :class="{ 'is-sparse': showSparseWorkbench, 'is-single': showSingleTokenWorkbench }"
  >
    <div class="rr-admin-workbench__layout">
      <aside class="rr-admin-workbench__rail">
        <header class="rr-admin-workbench__pane-head">
          <div class="rr-admin-workbench__pane-copy">
            <h3>{{ $t('admin.tokens.title') }}</h3>
            <p>{{ $t('admin.tokens.subtitle') }}</p>
          </div>
          <button
            v-if="!showEmptyState"
            class="rr-button"
            type="button"
            :disabled="loading"
            @click="emit('create')"
          >
            {{ $t('admin.createToken') }}
          </button>
        </header>

        <SearchField
          v-model="searchQuery"
          :placeholder="$t('admin.tokens.searchPlaceholder')"
          @clear="searchQuery = ''"
        />

        <div
          v-if="showSummary"
          class="rr-admin-workbench__summary"
        >
          <article class="rr-admin-workbench__metric">
            <strong>{{ summary.total }}</strong>
            <span>{{ $t('admin.tokens.summary.total') }}</span>
          </article>
          <article class="rr-admin-workbench__metric">
            <strong>{{ summary.active }}</strong>
            <span>{{ $t('admin.tokens.summary.active') }}</span>
          </article>
          <article class="rr-admin-workbench__metric">
            <strong>{{ summary.expiringSoon }}</strong>
            <span>{{ $t('admin.tokens.summary.expiringSoon') }}</span>
          </article>
        </div>

        <p
          v-if="errorMessage"
          class="rr-admin-workbench__feedback rr-admin-workbench__feedback--error"
        >
          {{ errorMessage }}
        </p>

        <div
          v-if="showLoadingState"
          class="rr-admin-workbench__state"
        >
          {{ $t('admin.loading') }}
        </div>
        <div
          v-else-if="showEmptyState"
          class="rr-admin-workbench__state"
        >
          {{ $t('admin.tokens.empty') }}
        </div>
        <div
          v-else-if="showNoResultsState"
          class="rr-admin-workbench__state"
        >
          {{ $t('shared.feedbackState.noResults') }}
        </div>
        <div
          v-else
          class="rr-admin-workbench__list"
        >
          <button
            v-for="row in filteredRows"
            :key="row.principalId"
            type="button"
            class="rr-admin-workbench__row"
            :class="{ 'rr-admin-workbench__row--active': selectedPrincipalId === row.principalId }"
            @click="selectRow(row.principalId)"
          >
            <div class="rr-admin-workbench__row-head">
              <strong>{{ row.label }}</strong>
              <span
                class="rr-status-pill"
                :class="statusClass(row.status)"
              >
                {{ statusBadgeLabel(row.status) }}
              </span>
            </div>

            <span class="rr-admin-workbench__row-subtitle">
              {{
                currentPrincipalId && row.principalId === currentPrincipalId
                  ? $t('admin.tokens.currentPrincipal')
                  : principalLabel(row)
              }}
              · {{ row.tokenPrefix }}
            </span>

            <div class="rr-admin-workbench__row-meta">
              <span>{{ scopeSummary(row) }}</span>
              <span>{{ secondaryMeta(row) }}</span>
            </div>
          </button>
        </div>
      </aside>

      <section class="rr-admin-workbench__detail">
        <div
          v-if="selectedRow"
          class="rr-admin-workbench__detail-card"
        >
          <header class="rr-admin-workbench__detail-head">
            <div class="rr-admin-workbench__pane-copy">
              <h3>{{ selectedRow.label }}</h3>
              <p>{{ selectedRow.tokenPrefix }}</p>
            </div>
            <span
              class="rr-status-pill"
              :class="statusClass(selectedRow.status)"
            >
              {{ statusBadgeLabel(selectedRow.status) }}
            </span>
          </header>

          <dl class="rr-admin-workbench__detail-grid">
            <div v-if="showPrincipalFact(selectedRow)">
              <dt>{{ $t('admin.headers.principal') }}</dt>
              <dd>{{ principalLabel(selectedRow) }}</dd>
            </div>
            <div>
              <dt>{{ $t('admin.tokens.scopeTitle') }}</dt>
              <dd>{{ scopeSummary(selectedRow) }}</dd>
            </div>
            <div>
              <dt>{{ $t('admin.headers.lastUsed') }}</dt>
              <dd>{{ activitySummary(selectedRow.lastUsedAt) }}</dd>
            </div>
            <div>
              <dt>{{ $t('admin.headers.expires') }}</dt>
              <dd>{{ activitySummary(selectedRow.expiresAt) }}</dd>
            </div>
            <div v-if="selectedRow.revokedAt">
              <dt>{{ $t('admin.tokens.revokedAt') }}</dt>
              <dd>{{ formatDateTime(selectedRow.revokedAt) }}</dd>
            </div>
          </dl>

          <section class="rr-admin-workbench__detail-section">
            <h4>{{ $t('admin.headers.grants') }}</h4>
            <ul class="rr-admin-token-detail__grant-list">
              <li
                v-for="grant in selectedRow.grants"
                :key="`${grant.resourceKind}:${grant.resourceId}:${grant.permissionKind}`"
              >
                <strong>{{ permissionLabel(grant.permissionKind) }}</strong>
                <span>{{ enumLabel('admin.tokens.resourceKinds', grant.resourceKind) }}</span>
              </li>
            </ul>
          </section>

          <p
            v-if="selectedRow.plaintextToken"
            class="rr-admin-workbench__feedback rr-admin-workbench__feedback--info"
          >
            {{ $t('admin.tokens.copyHint') }}
          </p>

          <div class="rr-admin-workbench__detail-actions">
            <button
              v-if="selectedRow.plaintextToken"
              class="rr-button rr-button--ghost"
              type="button"
              @click="emit('copy', selectedRow.principalId)"
            >
              {{ $t('admin.actions.copy') }}
            </button>
            <button
              v-if="selectedRow.status === 'active'"
              class="rr-button rr-button--ghost is-danger"
              type="button"
              @click="emit('revoke', selectedRow.principalId)"
            >
              {{ $t('admin.actions.revoke') }}
            </button>
          </div>
        </div>
        <div
          v-else-if="showEmptyState"
          class="rr-admin-workbench__detail-card"
        >
          <header class="rr-admin-workbench__detail-head">
            <div class="rr-admin-workbench__pane-copy">
              <h3>{{ $t('admin.tokens.emptyDetailTitle') }}</h3>
              <p>{{ $t('admin.tokens.emptyDetailDescription') }}</p>
            </div>
          </header>

          <p class="rr-admin-workbench__feedback rr-admin-workbench__feedback--info">
            {{ $t('admin.tokens.scope.library', { library: libraryName }) }}
          </p>

          <div class="rr-admin-workbench__detail-actions">
            <button
              class="rr-button"
              type="button"
              :disabled="loading"
              @click="emit('create')"
            >
              {{ $t('admin.createToken') }}
            </button>
          </div>
        </div>
        <div
          v-else-if="showNoResultsState"
          class="rr-admin-workbench__detail-card"
        >
          <header class="rr-admin-workbench__detail-head">
            <div class="rr-admin-workbench__pane-copy">
              <h3>{{ $t('shared.feedbackState.noResults') }}</h3>
              <p>{{ $t('admin.tokens.noResultsDetail') }}</p>
            </div>
          </header>

          <p class="rr-admin-workbench__feedback rr-admin-workbench__feedback--info">
            {{ $t('admin.tokens.searchPlaceholder') }}
          </p>

          <div class="rr-admin-workbench__detail-actions">
            <button
              class="rr-button rr-button--ghost"
              type="button"
              @click="clearSearch"
            >
              {{ $t('search.clear') }}
            </button>
          </div>
        </div>
        <div
          v-else
          class="rr-admin-workbench__state rr-admin-workbench__state--detail"
        >
          {{ $t('admin.tokens.emptyDetail') }}
        </div>
      </section>
    </div>
  </section>
</template>

<style scoped lang="scss">
.rr-admin-workbench--access.is-sparse .rr-admin-workbench__layout {
  min-height: 0;
  align-items: start;
}

.rr-admin-workbench--access.is-single .rr-admin-workbench__layout {
  min-height: 0;
  align-items: start;
}

@media (min-width: 981px) {
  .rr-admin-workbench--access.is-sparse .rr-admin-workbench__layout {
    grid-template-columns: minmax(320px, 24rem) minmax(360px, 42rem);
    justify-content: start;
  }

  .rr-admin-workbench--access.is-single .rr-admin-workbench__layout {
    grid-template-columns: minmax(300px, 24rem) minmax(380px, 44rem);
    justify-content: start;
  }
}

.rr-admin-workbench--access.is-sparse .rr-admin-workbench__rail,
.rr-admin-workbench--access.is-sparse .rr-admin-workbench__detail {
  height: auto;
  min-height: 0;
  align-self: start;
}

.rr-admin-workbench--access.is-single .rr-admin-workbench__rail,
.rr-admin-workbench--access.is-single .rr-admin-workbench__detail {
  height: auto;
  min-height: 0;
  align-self: start;
}

.rr-admin-workbench--access .rr-admin-workbench__detail-card {
  height: auto;
  min-height: 0;
  gap: 10px;
  padding: 16px;
}

.rr-admin-workbench--access .rr-admin-workbench__detail-grid {
  gap: 10px;
}

.rr-admin-workbench--access .rr-admin-workbench__detail-grid div {
  padding: 9px 11px;
}

.rr-admin-workbench--access .rr-admin-workbench__detail-section {
  gap: 6px;
}

.rr-admin-workbench--access .rr-admin-workbench__detail-actions {
  margin-top: 2px;
}

.rr-admin-workbench--access.is-sparse .rr-admin-workbench__detail-card {
  max-width: 42rem;
}

.rr-admin-workbench--access.is-single .rr-admin-workbench__detail-card {
  max-width: 44rem;
}

.rr-admin-token-detail__checklist,
.rr-admin-token-detail__grant-list {
  display: grid;
  gap: 0.7rem;
  margin: 0;
  padding: 0;
  list-style: none;
}

.rr-admin-token-detail__checklist li,
.rr-admin-token-detail__grant-list li {
  display: flex;
  flex-wrap: wrap;
  gap: 0.4rem 0.75rem;
  padding: 0.68rem 0.8rem;
  border-radius: 12px;
  background: rgba(248, 250, 252, 0.82);
  border: 1px solid rgba(226, 232, 240, 0.86);
}

.rr-admin-token-detail__grant-list strong {
  color: var(--rr-text-primary);
  font-size: 0.94rem;
}

.rr-admin-token-detail__checklist li {
  color: var(--rr-text-secondary);
  font-size: 0.88rem;
  line-height: 1.5;
}

.rr-admin-token-detail__grant-list span {
  color: var(--rr-text-secondary);
  font-size: 0.88rem;
}

@media (min-width: 1800px) {
  .rr-admin-workbench--access .rr-admin-workbench__detail-card {
    padding: 18px;
  }

  .rr-admin-token-detail__checklist,
  .rr-admin-token-detail__grant-list {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }
}
</style>
