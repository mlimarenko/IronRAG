<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { useDisplayFormatters } from 'src/composables/useDisplayFormatters'
import type { AdminAuditEvent } from 'src/models/ui/admin'

const props = defineProps<{
  events: AdminAuditEvent[]
}>()

const { t } = useI18n()
const { auditActionLabel, auditSubjectLabel, formatDateTime, humanizeToken, shortIdentifier } = useDisplayFormatters()

const visibleEvents = computed(() => props.events.slice(0, 8))

function subjectSummary(event: AdminAuditEvent): string {
  const first = event.subjects[0]
  if (!first) {
    return t('admin.audit.noSubjects')
  }
  return `${auditSubjectLabel(first.subjectKind)} · ${shortIdentifier(first.subjectId)}`
}

function actionSummary(event: AdminAuditEvent): string {
  return `${auditActionLabel(event.actionKind)} · ${humanizeToken(event.resultKind)}`
}

function eventMessage(event: AdminAuditEvent): string {
  return event.redactedMessage ?? event.internalMessage ?? actionSummary(event)
}
</script>

<template>
  <section class="rr-admin-audit-card">
    <header class="rr-admin-audit-card__head">
      <h3>{{ $t('admin.audit.title') }}</h3>
      <span>{{ visibleEvents.length }}</span>
    </header>

    <div
      v-if="visibleEvents.length"
      class="rr-admin-audit-card__list"
    >
      <article
        v-for="event in visibleEvents"
        :key="event.id"
        class="rr-admin-audit-card__item"
      >
        <div class="rr-admin-audit-card__meta">
          <strong>{{ actionSummary(event) }}</strong>
          <span>{{ formatDateTime(event.createdAt) }}</span>
        </div>
        <p>{{ eventMessage(event) }}</p>
        <div class="rr-admin-audit-card__foot">
          <span>{{ subjectSummary(event) }}</span>
          <span v-if="event.actorPrincipalId">
            {{ $t('admin.audit.actorLabel', { actor: shortIdentifier(event.actorPrincipalId) }) }}
          </span>
        </div>
      </article>
    </div>

    <p
      v-else
      class="rr-admin-audit-card__empty"
    >
      {{ $t('admin.audit.empty') }}
    </p>
  </section>
</template>

<style scoped>
.rr-admin-audit-card {
  display: grid;
  gap: 0.8rem;
}

.rr-admin-audit-card__head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 0.75rem;
}

.rr-admin-audit-card__head h3 {
  margin: 0;
  font-size: 1rem;
  color: var(--rr-text-primary);
}

.rr-admin-audit-card__head span,
.rr-admin-audit-card__meta span,
.rr-admin-audit-card__foot span,
.rr-admin-audit-card__empty {
  color: var(--rr-text-secondary);
  font-size: 0.88rem;
}

.rr-admin-audit-card__list {
  display: grid;
  gap: 0.75rem;
}

.rr-admin-audit-card__item {
  display: grid;
  gap: 0.35rem;
  padding: 0.8rem 0.85rem;
  border-radius: 16px;
  background: rgba(248, 250, 252, 0.72);
  border: 1px solid var(--rr-border-muted);
}

.rr-admin-audit-card__meta,
.rr-admin-audit-card__foot {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 0.75rem;
}

.rr-admin-audit-card__meta strong {
  color: var(--rr-text-primary);
  font-size: 0.94rem;
}

.rr-admin-audit-card__item p {
  margin: 0;
  color: var(--rr-text-secondary);
  font-size: 0.94rem;
  line-height: 1.45;
}

@media (max-width: 640px) {
  .rr-admin-audit-card__meta,
  .rr-admin-audit-card__foot {
    flex-direction: column;
    align-items: flex-start;
  }
}
</style>
