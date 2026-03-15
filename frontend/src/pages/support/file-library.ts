import type { DocumentSummary, IngestionJobDetail, SourceSummary } from 'src/boot/api'

export type FileHealthTone = 'positive' | 'warning' | 'negative' | 'info'
export type FileInventoryFilter = 'all' | 'recent' | 'attention' | 'manual' | 'upload'

export interface FileHealthSummary {
  tone: FileHealthTone
  label: string
  hint: string
}

export interface FileInventoryRecord {
  id: string
  title: string
  subtitle: string
  sourceLabel: string
  sourceKind: string
  sourceKindLabel: string
  statusLabel: string
  statusTone: FileHealthTone
  attention: boolean
  updatedAt: string | null
  updatedSortValue: number
  summaryLabel: string
  mimeLabel: string
  checksumShort: string | null
  routeQuery: {
    doc: string
  }
}

function normalizeStatus(value?: string | null): string {
  return (value ?? '').trim().toLowerCase()
}

function shortChecksum(value?: string | null): string | null {
  if (!value) {
    return null
  }

  return value.length > 10 ? `${value.slice(0, 10)}…` : value
}

export function formatLibraryDate(value?: string | null): string | null {
  if (!value) {
    return null
  }

  const date = new Date(value)
  if (Number.isNaN(date.getTime())) {
    return null
  }

  return new Intl.DateTimeFormat(undefined, {
    day: 'numeric',
    month: 'short',
    hour: '2-digit',
    minute: '2-digit',
  }).format(date)
}

export function sourceLabelForDocument(
  document: DocumentSummary,
  sources: SourceSummary[],
  fallback: string,
): { label: string; kind: string } {
  const source = sources.find((item) => item.id === document.source_id)
  if (!source) {
    return { label: fallback, kind: 'unknown' }
  }

  return {
    label: source.label,
    kind: source.source_kind,
  }
}

export function buildDocumentJobMap(jobs: IngestionJobDetail[]): Map<string, IngestionJobDetail> {
  const map = new Map<string, IngestionJobDetail>()

  for (const job of jobs) {
    if (!job.source_id || map.has(job.source_id)) {
      continue
    }

    map.set(job.source_id, job)
  }

  return map
}

export function describeDocumentHealth(
  document: DocumentSummary,
  relatedJob: IngestionJobDetail | null,
): FileHealthSummary {
  const status = normalizeStatus(document.status)
  const jobStatus = normalizeStatus(relatedJob?.status)

  if (jobStatus === 'failed' || jobStatus === 'retryable_failed' || jobStatus === 'canceled') {
    return {
      tone: 'warning',
      label: 'Needs attention',
      hint: 'Latest processing run did not finish cleanly.',
    }
  }

  if (status === 'indexed' || status === 'ready' || jobStatus === 'completed') {
    return {
      tone: 'positive',
      label: 'Searchable',
      hint: 'Indexed and available for search.',
    }
  }

  if (status === 'processing' || jobStatus === 'running' || jobStatus === 'queued') {
    return {
      tone: 'info',
      label: 'Processing',
      hint: 'Still moving through indexing.',
    }
  }

  if (status === 'failed') {
    return {
      tone: 'warning',
      label: 'Needs attention',
      hint: 'Document status indicates processing failed.',
    }
  }

  return {
    tone: 'info',
    label: 'Indexed',
    hint: 'Stored in the library inventory.',
  }
}

export function buildFileInventory(
  documents: DocumentSummary[],
  sources: SourceSummary[],
  jobs: IngestionJobDetail[],
  options: {
    unknownSourceLabel: string
    sourceKindFormatter: (value: string) => string
    statusFormatter: (value?: string | null) => string
    untitledLabel: string
    recentLabel: string
    checksumLabel: string
    mimeFallback: string
  },
): FileInventoryRecord[] {
  const jobBySourceId = buildDocumentJobMap(jobs)

  return documents.map((document) => {
    const sourceMeta = sourceLabelForDocument(document, sources, options.unknownSourceLabel)
    const relatedJob = document.source_id ? (jobBySourceId.get(document.source_id) ?? null) : null
    const health = describeDocumentHealth(document, relatedJob)
    const updatedAt = formatLibraryDate(relatedJob?.finished_at ?? relatedJob?.started_at ?? null)
    const updatedSortValue = relatedJob?.finished_at
      ? new Date(relatedJob.finished_at).getTime()
      : relatedJob?.started_at
        ? new Date(relatedJob.started_at).getTime()
        : 0

    const summaryParts = [health.hint]
    if (updatedAt) {
      summaryParts.push(`${options.recentLabel} ${updatedAt}`)
    }

    return {
      id: document.id,
      title: document.title?.trim() ?? options.untitledLabel,
      subtitle: document.external_key,
      sourceLabel: sourceMeta.label,
      sourceKind: sourceMeta.kind,
      sourceKindLabel: options.sourceKindFormatter(sourceMeta.kind),
      statusLabel: health.label,
      statusTone: health.tone,
      attention: health.tone === 'warning',
      updatedAt,
      updatedSortValue: Number.isFinite(updatedSortValue) ? updatedSortValue : 0,
      summaryLabel: summaryParts.join(' · '),
      mimeLabel: document.mime_type ?? options.mimeFallback,
      checksumShort: shortChecksum(document.checksum),
      routeQuery: {
        doc: document.id,
      },
    }
  })
}

export function matchesInventoryFilter(
  record: FileInventoryRecord,
  filter: FileInventoryFilter,
): boolean {
  switch (filter) {
    case 'recent':
      return record.updatedSortValue > 0
    case 'attention':
      return record.attention
    case 'manual':
      return record.sourceKind === 'text'
    case 'upload':
      return record.sourceKind === 'upload'
    default:
      return true
  }
}
