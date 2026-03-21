<script setup lang="ts">
import { computed } from 'vue'
import { useDisplayFormatters } from 'src/composables/useDisplayFormatters'
import type { AdminPrincipalSummary } from 'src/models/ui/admin'

const props = defineProps<{
  principal: AdminPrincipalSummary | null
  workspaceName: string
  libraryName: string
}>()
const { enumLabel } = useDisplayFormatters()

const visibleGrants = computed(() => props.principal?.effectiveGrants ?? [])
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
          {{ enumLabel('admin.tokens.principalKinds', principal.principalKind) }}
        </span>
      </header>

      <div class="rr-admin-banner__facts">
        <div>{{ principal.displayLabel }}</div>
        <div v-if="principal.email">{{ principal.email }}</div>
        <div>{{ $t('admin.tokens.scopeLine', { workspace: workspaceName, library: libraryName }) }}</div>
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
            {{ enumLabel('admin.tokens.resourceKinds', grant.resourceKind) }}
          </span>
        </div>
      </div>
    </div>
  </section>
</template>
