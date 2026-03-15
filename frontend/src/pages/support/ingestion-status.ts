import type { IngestionJobDetail, IngestionJobSummary } from 'src/boot/api'

type TranslateFn = (key: string, params?: Record<string, string | number>) => string

export type JobTone = 'positive' | 'warning' | 'negative' | 'info'

export interface IngestionErrorCopy {
  title: string
  body: string
  detail?: string
}

export interface JobPresentation {
  tone: JobTone
  statusLabel: string
  stageLabel: string
  summary: string
}

export interface JobStep {
  key: string
  label: string
  description: string
  state: 'complete' | 'active' | 'pending' | 'error'
}

function normalize(value?: string | null): string {
  return value?.trim().toLowerCase() ?? ''
}

export function shortJobId(id: string, length = 8): string {
  return id.slice(0, length)
}

export function ingestionLifecycleFromStatus(
  status?: string | null,
): IngestionJobDetail['lifecycle'] {
  switch (normalize(status)) {
    case 'queued':
      return 'Queued'
    case 'validating':
      return 'Validating'
    case 'running':
      return 'Running'
    case 'partial':
      return 'Partial'
    case 'completed':
      return 'Completed'
    case 'retryable_failed':
      return 'RetryableFailed'
    case 'canceled':
      return 'Canceled'
    default:
      return 'Failed'
  }
}

export function jobDetailFromSummary(summary: IngestionJobSummary): IngestionJobDetail {
  return {
    ...summary,
    requested_by: null,
    error_message: null,
    started_at: null,
    finished_at: null,
    retryable: summary.status === 'retryable_failed' || summary.status === 'partial',
    lifecycle: ingestionLifecycleFromStatus(summary.status),
  }
}

export function isActiveJobStatus(status?: string | null): boolean {
  return ['queued', 'validating', 'running', 'partial'].includes(normalize(status))
}

export function isTerminalJobStatus(status?: string | null): boolean {
  return ['completed', 'failed', 'retryable_failed', 'canceled'].includes(normalize(status))
}

export function formatJobStatus(status: string | null | undefined, t: TranslateFn): string {
  switch (normalize(status)) {
    case 'queued':
      return t('flow.library.processing.states.queued')
    case 'validating':
      return t('flow.library.processing.states.validating')
    case 'running':
      return t('flow.library.processing.states.running')
    case 'partial':
      return t('flow.library.processing.states.partial')
    case 'completed':
      return t('flow.library.processing.states.completed')
    case 'failed':
      return t('flow.library.processing.states.failed')
    case 'retryable_failed':
      return t('flow.library.processing.states.retryableFailed')
    case 'canceled':
      return t('flow.library.processing.states.canceled')
    default:
      return t('flow.library.processing.states.unknown')
  }
}

export function formatJobStage(stage: string | null | undefined, t: TranslateFn): string {
  switch (normalize(stage)) {
    case 'created':
      return t('flow.library.processing.stages.created')
    case 'claimed':
      return t('flow.library.processing.stages.claimed')
    case 'reclaimed_after_lease_expiry':
      return t('flow.library.processing.stages.reclaimedAfterLeaseExpiry')
    case 'requeued_after_lease_expiry':
      return t('flow.library.processing.stages.requeuedAfterLeaseExpiry')
    case 'persisting_document':
      return t('flow.library.processing.stages.persistingDocument')
    case 'chunking':
      return t('flow.library.processing.stages.chunking')
    case 'completed':
      return t('flow.library.processing.stages.completed')
    case 'failed':
      return t('flow.library.processing.stages.failed')
    default:
      return t('flow.library.processing.stages.unknown')
  }
}

export function formatTriggerKind(triggerKind: string | null | undefined, t: TranslateFn): string {
  switch (normalize(triggerKind)) {
    case 'text_ingest':
      return t('flow.library.processing.triggers.textIngest')
    case 'upload_ingest':
      return t('flow.library.processing.triggers.uploadIngest')
    case 'manual':
      return t('flow.library.processing.triggers.manual')
    default:
      return t('flow.library.processing.triggers.unknown')
  }
}

export function formatSourceKind(sourceKind: string | null | undefined, t: TranslateFn): string {
  switch (normalize(sourceKind)) {
    case 'text':
      return t('flow.library.lists.sources.kinds.text')
    case 'upload':
      return t('flow.library.lists.sources.kinds.upload')
    default:
      return t('flow.library.lists.sources.kinds.unknown')
  }
}

function getJobTone(status?: string | null): JobTone {
  switch (normalize(status)) {
    case 'completed':
      return 'positive'
    case 'failed':
    case 'retryable_failed':
    case 'canceled':
      return 'negative'
    case 'queued':
    case 'validating':
    case 'running':
    case 'partial':
      return 'warning'
    default:
      return 'info'
  }
}

export function describeIngestionJob(
  job: Pick<IngestionJobDetail, 'status' | 'stage'>,
  t: TranslateFn,
): JobPresentation {
  const status = normalize(job.status)
  const stage = normalize(job.stage)

  let summary = t('flow.library.processing.summaries.fallback')

  if (status === 'completed') {
    summary = t('flow.library.processing.summaries.completed')
  } else if (status === 'canceled') {
    summary = t('flow.library.processing.summaries.canceled')
  } else if (status === 'retryable_failed' || status === 'failed') {
    summary = t('flow.library.processing.summaries.failed')
  } else if (status === 'partial') {
    summary = t('flow.library.processing.summaries.partial')
  } else if (status === 'validating') {
    summary = t('flow.library.processing.summaries.validating')
  } else if (stage === 'claimed') {
    summary = t('flow.library.processing.summaries.claimed')
  } else if (stage === 'reclaimed_after_lease_expiry') {
    summary = t('flow.library.processing.summaries.reclaimed')
  } else if (stage === 'requeued_after_lease_expiry') {
    summary = t('flow.library.processing.summaries.requeued')
  } else if (stage === 'persisting_document') {
    summary = t('flow.library.processing.summaries.persisting')
  } else if (stage === 'chunking') {
    summary = t('flow.library.processing.summaries.chunking')
  } else if (stage === 'created' || status === 'queued') {
    summary = t('flow.library.processing.summaries.queued')
  }

  return {
    tone: getJobTone(job.status),
    statusLabel: formatJobStatus(job.status, t),
    stageLabel: formatJobStage(job.stage, t),
    summary,
  }
}

function getStageStepIndex(stage?: string | null, status?: string | null): number {
  const normalizedStage = normalize(stage)
  const normalizedStatus = normalize(status)

  if (normalizedStatus === 'completed' || normalizedStage === 'completed') {
    return 3
  }

  switch (normalizedStage) {
    case 'persisting_document':
      return 1
    case 'chunking':
    case 'failed':
      return 2
    case 'claimed':
    case 'created':
    case 'reclaimed_after_lease_expiry':
    case 'requeued_after_lease_expiry':
      return 0
    default:
      return normalizedStatus === 'running' || normalizedStatus === 'partial' ? 2 : 0
  }
}

export function buildJobSteps(
  job: Pick<IngestionJobDetail, 'status' | 'stage'>,
  t: TranslateFn,
): JobStep[] {
  const status = normalize(job.status)
  const hasFailed = ['failed', 'retryable_failed', 'canceled'].includes(status)
  const currentIndex = getStageStepIndex(job.stage, job.status)
  const completed = status === 'completed'
  const keys = ['queued', 'persisted', 'chunked', 'ready'] as const

  return keys.map((key, index) => {
    let state: JobStep['state'] = 'pending'

    if (completed || index < currentIndex) {
      state = 'complete'
    } else if (hasFailed && index === currentIndex) {
      state = 'error'
    } else if (index === currentIndex) {
      state = 'active'
    }

    return {
      key,
      label: t(`flow.library.processing.steps.${key}.label`),
      description: t(`flow.library.processing.steps.${key}.description`),
      state,
    }
  })
}

export function describeIngestionError(rawMessage: string, t: TranslateFn): IngestionErrorCopy {
  const message = rawMessage.trim()
  const normalized = message.toLowerCase()

  if (
    normalized.includes('already exists for this idempotency key') ||
    normalized.includes('conflict')
  ) {
    return {
      title: t('flow.library.notices.duplicateTitle'),
      body: t('flow.library.notices.duplicateBody'),
      detail: message,
    }
  }

  if (
    normalized.includes('text must not be empty') ||
    normalized.includes('uploaded file is empty')
  ) {
    return {
      title: t('flow.library.notices.emptyTitle'),
      body: t('flow.library.notices.emptyBody'),
      detail: message,
    }
  }

  if (normalized.includes('missing file')) {
    return {
      title: t('flow.library.notices.uploadTypeTitle'),
      body: t('flow.library.notices.emptyBody'),
      detail: message,
    }
  }

  if (normalized.includes('pdf uploads are planned')) {
    return {
      title: t('flow.library.notices.pdfTitle'),
      body: t('flow.library.notices.pdfBody'),
      detail: message,
    }
  }

  if (normalized.includes('image uploads are planned')) {
    return {
      title: t('flow.library.notices.imageTitle'),
      body: t('flow.library.notices.imageBody'),
      detail: message,
    }
  }

  if (
    normalized.includes('utf-8 text-like uploads are supported') ||
    normalized.includes('could not be decoded as utf-8') ||
    normalized.includes('choose a utf-8 text-like file')
  ) {
    return {
      title: t('flow.library.notices.uploadTypeTitle'),
      body: t('flow.library.notices.uploadTypeBody'),
      detail: message,
    }
  }

  if (
    normalized.includes('authorization') ||
    normalized.includes('401') ||
    normalized.includes('unauthorized')
  ) {
    return {
      title: t('flow.library.notices.authTitle'),
      body: t('flow.library.notices.authBody'),
      detail: message,
    }
  }

  if (normalized.includes('payload missing or invalid')) {
    return {
      title: t('flow.library.notices.genericErrorTitle'),
      body: t('flow.library.notices.payloadBody'),
      detail: message,
    }
  }

  if (normalized.includes('lease expired')) {
    return {
      title: t('flow.library.notices.jobLeaseTitle'),
      body: t('flow.library.notices.jobLeaseBody'),
      detail: message,
    }
  }

  return {
    title: t('flow.library.notices.genericErrorTitle'),
    body: t('flow.library.notices.genericErrorBody'),
    detail: message,
  }
}
