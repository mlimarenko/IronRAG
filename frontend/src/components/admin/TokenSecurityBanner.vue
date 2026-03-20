<script setup lang="ts">
import { computed } from 'vue'
import type { AdminPrincipalSummary } from 'src/models/ui/admin'

const props = defineProps<{
  principal: AdminPrincipalSummary | null
  workspaceName: string
  libraryName: string
}>()

const visibleGrants = computed(() => props.principal?.effectiveGrants ?? [])

function shortId(value: string | null): string {
  if (!value) {
    return '—'
  }
  return value.slice(0, 8)
}
</script>

<template>
  <section class="rr-admin-banner">
    <div class="rr-admin-banner__copy">
      <strong>{{ $t('admin.security.title') }}</strong>
      <p>{{ $t('admin.security.body') }}</p>
    </div>

    <div
      v-if="principal"
      class="rr-admin-banner__principal"
    >
      <header>
        <strong>{{ $t('admin.tokens.currentPrincipal') }}</strong>
        <span class="rr-status-pill is-configured">
          {{ principal.principalKind }}
        </span>
      </header>

      <div class="rr-admin-banner__facts">
        <span>{{ principal.displayLabel }}</span>
        <span><code>{{ shortId(principal.id) }}</code></span>
        <span v-if="principal.email">{{ principal.email }}</span>
        <span>{{ $t('admin.tokens.scopeLine', { workspace: workspaceName, library: libraryName }) }}</span>
      </div>

      <div class="rr-admin-banner__grants">
        <strong>{{ $t('admin.tokens.currentGrants') }}</strong>
        <div class="rr-admin-scopes">
          <span
            v-for="grant in visibleGrants"
            :key="grant.id"
            class="rr-admin-scope-pill"
          >
            {{ $t(`admin.tokens.permissions.${grant.permissionKind}`) }}
            ·
            {{ grant.resourceKind }}
            <template v-if="grant.resourceKind !== 'system'">
              :{{ shortId(grant.resourceId) }}
            </template>
          </span>
        </div>
      </div>
    </div>
  </section>
</template>
