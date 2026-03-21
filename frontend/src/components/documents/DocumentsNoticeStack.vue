<script setup lang="ts">
import type { DocumentsWorkspaceNotice } from 'src/models/ui/documents'

defineProps<{
  degraded: DocumentsWorkspaceNotice[]
  informational: DocumentsWorkspaceNotice[]
}>()
</script>

<template>
  <section
    v-if="degraded.length || informational.length"
    class="rr-documents-notice-stack"
  >
    <div
      v-if="degraded.length"
      class="rr-documents-notice-stack__group is-degraded"
    >
      <article
        v-for="notice in degraded"
        :key="`${notice.kind}:${notice.message}`"
        class="rr-documents-notice-stack__notice"
        :class="{
          'is-provider-failure': notice.kind.includes('provider_failure'),
          'is-residual': notice.kind.startsWith('residual:'),
          'is-readiness': notice.kind.startsWith('readiness:'),
        }"
      >
        <strong>{{ notice.title }}</strong>
        <span
          v-if="notice.kind.startsWith('readiness:')"
          class="rr-documents-notice-stack__badge"
        >
          Knowledge readiness
        </span>
        <p>{{ notice.message }}</p>
      </article>
    </div>

    <div
      v-if="informational.length"
      class="rr-documents-notice-stack__group"
    >
      <article
        v-for="notice in informational"
        :key="`${notice.kind}:${notice.message}`"
        class="rr-documents-notice-stack__notice"
        :class="{
          'is-residual': notice.kind.startsWith('residual:'),
          'is-readiness': notice.kind.startsWith('readiness:'),
        }"
      >
        <strong>{{ notice.title }}</strong>
        <span
          v-if="notice.kind.startsWith('readiness:')"
          class="rr-documents-notice-stack__badge"
        >
          Knowledge readiness
        </span>
        <p>{{ notice.message }}</p>
      </article>
    </div>
  </section>
</template>
