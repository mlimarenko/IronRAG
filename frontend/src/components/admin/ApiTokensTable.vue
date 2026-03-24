<script setup lang="ts">
import { useI18n } from 'vue-i18n'
import { useDisplayFormatters } from 'src/composables/useDisplayFormatters'
import type { AdminApiTokenRow } from 'src/models/ui/admin'

const props = defineProps<{
  rows: AdminApiTokenRow[]
  currentPrincipalId: string | null
  currentPrincipalLabel: string | null
  embedded?: boolean
}>()

const emit = defineEmits<{
  create: []
  copy: [principalId: string]
  revoke: [principalId: string]
}>()
const { t } = useI18n()
const { enumLabel, formatDateTime, permissionLabel, statusBadgeLabel } = useDisplayFormatters()

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

</script>

<template>
  <section
    class="rr-admin-table"
    :class="{ 'rr-page-card': !embedded, 'rr-admin-table--embedded': embedded }"
  >
    <div
      v-if="!embedded"
      class="rr-admin-table__header"
    >
      <div>
        <h3>{{ $t('admin.tokens.title') }}</h3>
        <p>{{ $t('admin.tokens.subtitle') }}</p>
      </div>

      <button
        class="rr-button"
        type="button"
        @click="emit('create')"
      >
        {{ $t('admin.createToken') }}
      </button>
    </div>

    <div
      v-if="rows.length > 0"
      class="rr-admin-settings__list"
    >
      <article
        v-for="row in rows"
        :key="row.principalId"
        class="rr-admin-settings__list-row"
      >
        <div class="rr-admin-settings__list-main">
          <strong>{{ row.label }}</strong>
          <span>
            {{
              currentPrincipalId && row.principalId === currentPrincipalId
                ? $t('admin.tokens.currentPrincipal')
                : principalLabel(row)
            }}
            · {{ row.tokenPrefix }}
          </span>
          <span>{{ grantsSummary(row) }}</span>
        </div>

        <div class="rr-admin-settings__list-meta">
          <span
            class="rr-status-pill"
            :class="statusClass(row.status)"
          >
            {{ statusBadgeLabel(row.status) }}
          </span>
          <span>{{ $t('admin.headers.lastUsed') }}: {{ activitySummary(row.lastUsedAt) }}</span>
          <span>{{ $t('admin.headers.expires') }}: {{ activitySummary(row.expiresAt) }}</span>
          <span v-if="row.revokedAt">{{ formatDateTime(row.revokedAt) }}</span>
        </div>

        <div class="rr-admin-settings__row-actions">
          <button
            v-if="row.plaintextToken"
            class="rr-button rr-button--ghost rr-button--tiny"
            type="button"
            @click="emit('copy', row.principalId)"
          >
            {{ $t('admin.actions.copy') }}
          </button>
          <button
            v-if="row.status === 'active'"
            class="rr-button rr-button--ghost rr-button--tiny is-danger"
            type="button"
            @click="emit('revoke', row.principalId)"
          >
            {{ $t('admin.actions.revoke') }}
          </button>
        </div>
      </article>
    </div>

    <p
      v-else
      class="rr-admin-table__empty"
    >
      {{ $t('admin.tokens.empty') }}
    </p>
  </section>
</template>

<style scoped lang="scss">
.rr-admin-table__empty {
  margin: 0;
  color: var(--rr-text-secondary);
  font-size: 0.95rem;
  line-height: 1.55;
}

.rr-admin-settings__list-main strong {
  font-size: 1rem;
}

.rr-admin-settings__list-main span,
.rr-admin-settings__list-meta span {
  font-size: 0.9rem;
  line-height: 1.5;
}
</style>
