import { Download, LoaderCircle } from 'lucide-react'
import type { ReactNode } from 'react'
import { useTranslation } from 'react-i18next'
import {
  mimeTypeLabel,
  revisionSourceKindLabel,
  stageStatusLabel,
  structuredBlockKindLabel,
  technicalFactKindLabel,
  humanizeToken,
} from '@/shared/lib/enumLabels'

import type { DocumentLifecycleDetail } from '@/shared/api'
import type { Locale } from '@/shared/types'
import { formatDate } from '@/shared/format'
import { Badge, Button, ScreenState } from '@/shared/ui'
import { humanizeDocumentStage, stageTone } from '@/shared/lib/document-processing'

import type { DocumentInspectorController } from './useDocumentInspectorController'

export function PipelineSection({
  lifecycle,
  locale,
}: Readonly<{
  lifecycle: DocumentLifecycleDetail | null
  locale: Locale
}>) {
  const { t } = useTranslation()
  const attempt = lifecycle?.attempts[0]
  if (!attempt) return <SectionState text={t('documents.pipelineNoAttempts')} />
  return (
    <div className="stage-list">
      {attempt.stageEvents.map((event, index) => (
        <div key={`${event.stage}-${event.startedAt}`} className={`stage stage--${event.status}`}>
          <span className="stage-index">{index + 1}</span>
          <div>
            <strong>{humanizeDocumentStage(event.stage, t) ?? event.stage}</strong>
            <small>
              {event.modelName ??
                (event.providerKind ? humanizeToken(event.providerKind) : null) ??
                formatDate(event.startedAt, locale, { dateStyle: 'short', timeStyle: 'short' })}
            </small>
          </div>
          <span>{event.elapsedMs == null ? '—' : `${(event.elapsedMs / 1000).toFixed(1)}s`}</span>
          <Badge tone={stageTone(event.status)}>{stageStatusLabel(event.status, t)}</Badge>
        </div>
      ))}
    </div>
  )
}

export function RevisionsSection({
  controller,
  locale,
}: Readonly<{
  controller: DocumentInspectorController
  locale: Locale
}>) {
  const { t } = useTranslation()
  if (controller.revisions.isLoading) return <SectionState loading text={t('common.loading')} />
  if (controller.revisions.error) {
    const message = controller.errorMessage(controller.revisions.error, t('documents.failedToLoad'))
    return (
      <SectionState
        text={message}
        title={message}
        action={
          <Button size="sm" onClick={() => void controller.revisions.refetch()}>
            {t('documents.retry')}
          </Button>
        }
      />
    )
  }
  const revisions = controller.revisions.data ?? []
  if (!revisions.length) return <EmptySectionState text={t('common.empty')} />
  return (
    <div className="revision-list">
      {revisions.map((revision) => (
        <div key={revision.id}>
          <span>v{revision.revision_number}</span>
          <div>
            <strong>{revisionSourceKindLabel(revision.content_source_kind, t)}</strong>
            <small>
              {formatDate(revision.created_at, locale, { dateStyle: 'medium', timeStyle: 'short' })}{' '}
              · {mimeTypeLabel(revision.mime_type, t)}
            </small>
          </div>
          <Button
            size="icon"
            variant="ghost"
            onClick={() =>
              void controller.download({
                id: revision.id,
                revisionNumber: revision.revision_number,
              })
            }
            aria-label={t('documents.downloadRevision', { number: revision.revision_number })}
          >
            <Download size={14} />
          </Button>
        </div>
      ))}
    </div>
  )
}

export function SegmentsSection({
  controller,
}: Readonly<{ controller: DocumentInspectorController }>) {
  const { t } = useTranslation()
  if (controller.segments.isLoading) return <SectionState loading text={t('common.loading')} />
  if (controller.segments.error) {
    const message = controller.errorMessage(controller.segments.error, t('documents.failedToLoad'))
    return (
      <SectionState
        text={message}
        title={message}
        action={
          <Button size="sm" onClick={() => void controller.segments.refetch()}>
            {t('documents.retry')}
          </Button>
        }
      />
    )
  }
  const page = controller.segments.data
  if (!page?.items.length) return <EmptySectionState text={t('common.empty')} />
  return (
    <>
      <div className="segment-list">
        {(page?.items ?? []).map((item) => (
          <details key={item.segment.segmentId}>
            <summary>
              <span>{item.segment.headingTrail.at(-1) ?? item.segment.excerpt}</span>
              <Badge>{structuredBlockKindLabel(item.segment.blockKind, t)}</Badge>
            </summary>
            <p>{item.text || item.normalizedText}</p>
            <small>
              {item.segment.pageNumber
                ? `${t('common.page')} ${item.segment.pageNumber}`
                : `#${item.segment.ordinal + 1}`}
            </small>
          </details>
        ))}
      </div>
      <OffsetPager
        offset={controller.segmentOffset}
        limit={controller.PAGE_LIMIT}
        total={page?.total ?? 0}
        setOffset={controller.setSegmentOffset}
      />
    </>
  )
}

export function FactsSection({
  controller,
}: Readonly<{ controller: DocumentInspectorController }>) {
  const { t } = useTranslation()
  if (controller.facts.isLoading) return <SectionState loading text={t('common.loading')} />
  if (controller.facts.error) {
    const message = controller.errorMessage(controller.facts.error, t('documents.failedToLoad'))
    return (
      <SectionState
        text={message}
        title={message}
        action={
          <Button size="sm" onClick={() => void controller.facts.refetch()}>
            {t('documents.retry')}
          </Button>
        }
      />
    )
  }
  const page = controller.facts.data
  if (!page?.items.length) return <EmptySectionState text={t('common.empty')} />
  return (
    <>
      <div className="fact-grid">
        {(page?.items ?? []).map((fact) => (
          <div key={fact.factId}>
            <Badge>{technicalFactKindLabel(fact.factKind, t)}</Badge>
            <code>{fact.displayValue}</code>
            <span>{fact.confidence == null ? '—' : fact.confidence.toFixed(2)}</span>
            {fact.qualifiers.length > 0 && (
              <small>
                {fact.qualifiers
                  .map((qualifier) => `${humanizeToken(qualifier.key)}: ${qualifier.value}`)
                  .join(' · ')}
              </small>
            )}
          </div>
        ))}
      </div>
      <OffsetPager
        offset={controller.factOffset}
        limit={controller.PAGE_LIMIT}
        total={page?.total ?? 0}
        setOffset={controller.setFactOffset}
      />
    </>
  )
}

function OffsetPager({
  offset,
  limit,
  total,
  setOffset,
}: Readonly<{
  offset: number
  limit: number
  total: number
  setOffset: (offset: number) => void
}>) {
  const { t } = useTranslation()
  if (total <= limit) return null
  return (
    <div className="inspector-pager">
      <Button
        size="sm"
        disabled={offset === 0}
        onClick={() => setOffset(Math.max(0, offset - limit))}
      >
        {t('documents.previous')}
      </Button>
      <span>
        {Math.min(offset + 1, total)}–{Math.min(offset + limit, total)} / {total}
      </span>
      <Button
        size="sm"
        disabled={offset + limit >= total}
        onClick={() => setOffset(offset + limit)}
      >
        {t('documents.next')}
      </Button>
    </div>
  )
}

function EmptySectionState({ text }: Readonly<{ text: string }>) {
  return (
    <ScreenState scenario="empty" emptyText={text}>
      {null}
    </ScreenState>
  )
}

function SectionState({
  action,
  loading = false,
  text,
  title,
}: Readonly<{
  action?: ReactNode
  loading?: boolean
  text: string
  title?: string
}>) {
  return (
    <div className="inspector-section-state" title={title}>
      {loading && <LoaderCircle className="spin" size={16} />}
      <span>{text}</span>
      {action}
    </div>
  )
}
