import type { ComposerTranslation } from 'vue-i18n'

import type { ProjectReadinessSummary } from 'src/boot/api'

export type ProjectReadinessTone = 'positive' | 'warning' | 'info'

export interface ProjectReadinessPresentation {
  stateLabel: string
  askLabel: string
  askHint: string
  libraryHint: string
  freshnessHint: string | null
  tone: ProjectReadinessTone
  queryable: boolean
  hasAnyDocuments: boolean
  hasActiveJobs: boolean
  hasFailures: boolean
}

export function formatProjectReadiness(
  readiness: ProjectReadinessSummary | null,
  t: ComposerTranslation,
): ProjectReadinessPresentation {
  if (!readiness) {
    return {
      stateLabel: t('flow.readiness.states.unknown'),
      askLabel: t('flow.readiness.ask.unavailable'),
      askHint: t('flow.readiness.askHints.chooseLibrary'),
      libraryHint: t('flow.readiness.libraryHints.unknown'),
      freshnessHint: null,
      tone: 'info',
      queryable: false,
      hasAnyDocuments: false,
      hasActiveJobs: false,
      hasFailures: false,
    }
  }

  const state = readiness.indexing_state.trim()
  const hasAnyDocuments = readiness.documents > 0
  const hasActiveJobs = (readiness.active_ingestion_jobs ?? 0) > 0
  const hasFailures = (readiness.failed_ingestion_jobs ?? 0) > 0
  const latestFailed = ['failed', 'retryable_failed', 'canceled'].includes(
    readiness.latest_ingestion_status ?? '',
  )
  const queryable = readiness.ready_for_query

  if (queryable && hasFailures) {
    return {
      stateLabel: t('flow.readiness.states.indexedWithWarnings'),
      askLabel: t('flow.readiness.ask.available'),
      askHint: latestFailed
        ? t('flow.readiness.askHints.availableLatestFailed')
        : t('flow.readiness.askHints.availableWithWarnings'),
      libraryHint: t('flow.readiness.libraryHints.indexedWithWarnings'),
      freshnessHint: latestFailed
        ? t('flow.readiness.freshness.latestFailed')
        : t('flow.readiness.freshness.hadFailures'),
      tone: 'warning',
      queryable,
      hasAnyDocuments,
      hasActiveJobs,
      hasFailures,
    }
  }

  if (queryable) {
    return {
      stateLabel: t('flow.readiness.states.ready'),
      askLabel: t('flow.readiness.ask.available'),
      askHint: t('flow.readiness.askHints.available'),
      libraryHint: t('flow.readiness.libraryHints.ready'),
      freshnessHint: null,
      tone: 'positive',
      queryable,
      hasAnyDocuments,
      hasActiveJobs,
      hasFailures,
    }
  }

  if (hasAnyDocuments && hasActiveJobs) {
    return {
      stateLabel: t('flow.readiness.states.partial'),
      askLabel: t('flow.readiness.ask.almost'),
      askHint: t('flow.readiness.askHints.partial'),
      libraryHint: t('flow.readiness.libraryHints.partial'),
      freshnessHint: t('flow.readiness.freshness.processing'),
      tone: 'warning',
      queryable,
      hasAnyDocuments,
      hasActiveJobs,
      hasFailures,
    }
  }

  if (hasAnyDocuments) {
    return {
      stateLabel:
        state === 'stale' ? t('flow.readiness.states.stale') : t('flow.readiness.states.ready'),
      askLabel: t('flow.readiness.ask.availableSoon'),
      askHint: t('flow.readiness.askHints.documentsPresent'),
      libraryHint:
        state === 'stale'
          ? t('flow.readiness.libraryHints.stale')
          : t('flow.readiness.libraryHints.documentsPresent'),
      freshnessHint:
        state === 'stale'
          ? t('flow.readiness.freshness.stale')
          : t('flow.readiness.freshness.documentsPresent'),
      tone: 'warning',
      queryable,
      hasAnyDocuments,
      hasActiveJobs,
      hasFailures,
    }
  }

  return {
    stateLabel: t('flow.readiness.states.empty'),
    askLabel: t('flow.readiness.ask.unavailable'),
    askHint: t('flow.readiness.askHints.empty'),
    libraryHint: t('flow.readiness.libraryHints.empty'),
    freshnessHint: null,
    tone: 'info',
    queryable,
    hasAnyDocuments,
    hasActiveJobs,
    hasFailures,
  }
}
