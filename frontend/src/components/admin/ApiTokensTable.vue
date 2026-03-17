<script setup lang="ts">
import type { ApiTokenRow } from 'src/models/ui/admin'

defineProps<{
  rows: ApiTokenRow[]
}>()

const emit = defineEmits<{
  copy: [id: string]
  revoke: [id: string]
}>()

function formatDate(value: string | null): string {
  if (!value) {
    return 'Never'
  }
  const parsed = new Date(value)
  if (Number.isNaN(parsed.getTime())) {
    return value
  }
  return parsed.toLocaleDateString()
}
</script>

<template>
  <section class="rr-page-card rr-admin-table">
    <table>
      <thead>
        <tr>
          <th>{{ $t('admin.headers.label') }}</th>
          <th>{{ $t('admin.headers.token') }}</th>
          <th>{{ $t('admin.headers.scopes') }}</th>
          <th>{{ $t('admin.headers.created') }}</th>
          <th>{{ $t('admin.headers.lastUsed') }}</th>
          <th>{{ $t('admin.headers.expires') }}</th>
          <th>{{ $t('admin.headers.actions') }}</th>
        </tr>
      </thead>
      <tbody>
        <tr
          v-for="row in rows"
          :key="row.id"
        >
          <td>{{ row.label }}</td>
          <td>
            <div class="rr-admin-token-cell">
              <code>{{ row.maskedToken }}</code>
              <button
                v-if="row.plaintextToken"
                class="rr-button rr-button--ghost rr-button--tiny"
                type="button"
                @click="emit('copy', row.id)"
              >
                {{ $t('admin.actions.copy') }}
              </button>
            </div>
          </td>
          <td>
            <div class="rr-admin-scopes">
              <span
                v-for="scope in row.scopes"
                :key="scope"
                class="rr-admin-scope-pill"
              >
                {{ scope }}
              </span>
            </div>
          </td>
          <td>{{ formatDate(row.createdAt) }}</td>
          <td>{{ formatDate(row.lastUsedAt) }}</td>
          <td>{{ formatDate(row.expiresAt) }}</td>
          <td>
            <button
              v-if="row.canRevoke"
              class="rr-button rr-button--ghost rr-button--tiny is-danger"
              type="button"
              @click="emit('revoke', row.id)"
            >
              {{ $t('admin.actions.revoke') }}
            </button>
          </td>
        </tr>
      </tbody>
    </table>
  </section>
</template>
