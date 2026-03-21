<script setup lang="ts">
import { useDisplayFormatters } from 'src/composables/useDisplayFormatters'
import type { AdminApiTokenRow } from 'src/models/ui/admin'

const props = defineProps<{
  rows: AdminApiTokenRow[]
  currentPrincipalId: string | null
  currentPrincipalLabel: string | null
  workspaceName: string
}>()

const emit = defineEmits<{
  copy: [principalId: string]
  revoke: [principalId: string]
}>()
const { enumLabel, formatDateTime } = useDisplayFormatters()

function principalLabel(row: AdminApiTokenRow): string {
  if (props.currentPrincipalId && row.principalId === props.currentPrincipalId) {
    return props.currentPrincipalLabel?.trim() || row.label
  }
  return enumLabel('admin.tokens.principalKinds', 'api_token')
}

function workspaceLabel(row: AdminApiTokenRow): string {
  if (!row.workspaceId) {
    return '—'
  }
  return props.workspaceName
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

</script>

<template>
  <section class="rr-page-card rr-admin-table">
    <div class="rr-admin-table__header">
      <div>
        <h3>{{ $t('admin.tokens.title') }}</h3>
        <p>{{ $t('admin.tokens.subtitle') }}</p>
      </div>
    </div>

    <table v-if="rows.length > 0">
      <thead>
        <tr>
          <th>{{ $t('admin.headers.label') }}</th>
          <th>{{ $t('admin.headers.principal') }}</th>
          <th>{{ $t('admin.headers.workspace') }}</th>
          <th>{{ $t('admin.headers.grants') }}</th>
          <th>{{ $t('admin.headers.token') }}</th>
          <th>{{ $t('admin.headers.lifecycle') }}</th>
          <th>{{ $t('admin.headers.lastUsed') }}</th>
          <th>{{ $t('admin.headers.expires') }}</th>
          <th>{{ $t('admin.headers.actions') }}</th>
        </tr>
      </thead>
      <tbody>
        <tr
          v-for="row in rows"
          :key="row.principalId"
        >
          <td>{{ row.label }}</td>
          <td>
            <div class="rr-admin-token-status">
              <span>{{ principalLabel(row) }}</span>
              <small v-if="currentPrincipalId && row.principalId === currentPrincipalId">
                {{ $t('admin.tokens.currentPrincipal') }}
              </small>
            </div>
          </td>
          <td>
            {{ workspaceLabel(row) }}
          </td>
          <td>
            <div
              v-if="row.grants.length > 0"
              class="rr-admin-token-grants"
            >
              <span
                v-for="grant in row.grants"
                :key="grant.id"
                class="rr-status-pill is-muted"
              >
                {{ $t(`admin.tokens.permissions.${grant.permissionKind}`) }}
                ·
                {{ enumLabel('admin.tokens.resourceKinds', grant.resourceKind) }}
              </span>
            </div>
            <span v-else>—</span>
          </td>
          <td>
            <div class="rr-admin-token-cell">
              <code>{{ row.tokenPrefix }}</code>
              <button
                v-if="row.plaintextToken"
                class="rr-button rr-button--ghost rr-button--tiny"
                type="button"
                @click="emit('copy', row.principalId)"
              >
                {{ $t('admin.actions.copy') }}
              </button>
            </div>
          </td>
          <td>
            <div class="rr-admin-token-status">
              <span
                class="rr-status-pill"
                :class="statusClass(row.status)"
              >
                {{ $t(`admin.tokens.status.${row.status}`) }}
              </span>
              <small v-if="row.revokedAt">{{ formatDateTime(row.revokedAt) }}</small>
            </div>
          </td>
          <td>{{ formatDateTime(row.lastUsedAt) }}</td>
          <td>{{ formatDateTime(row.expiresAt) }}</td>
          <td>
            <button
              v-if="row.status === 'active'"
              class="rr-button rr-button--ghost rr-button--tiny is-danger"
              type="button"
              @click="emit('revoke', row.principalId)"
            >
              {{ $t('admin.actions.revoke') }}
            </button>
          </td>
        </tr>
      </tbody>
    </table>

    <p
      v-else
      class="rr-admin-table__empty"
    >
      {{ $t('admin.tokens.empty') }}
    </p>
  </section>
</template>
