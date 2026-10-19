import { ExternalLink, FileText, Network } from 'lucide-react'
import type { TFunction } from 'i18next'
import { useMemo, useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { useTranslation } from 'react-i18next'

import type { AssistantEvidenceView } from '@/features/assistant/model/assistantAdapter'
import { useApp } from '@/shared/contexts/app-context'
import { humanizeToken, technicalFactKindLabel } from '@/shared/lib/enumLabels'
import { Badge, Button, Drawer, ScreenState } from '@/shared/ui'
import type { EvidenceBundle } from '@/shared/types'

type EvidenceDrawerProps = Readonly<{
  evidence: EvidenceBundle | null
  open: boolean
  onOpenChange: (open: boolean) => void
}>

type EvidenceItem = {
  detail?: string
  documentId?: string
  entityType?: string
  href?: string
  id: string
  nodeId?: string
  rank: number
  score: number
  sectionPath?: string
  sourceKind?: 'external_url' | 'stored_document'
  title: string
  type: 'chunk' | 'segment' | 'fact' | 'entity' | 'relation'
}

const REFERENCES_PAGE_SIZE = 12

function relevanceLabel(value: number, locale: string): string {
  if (!Number.isFinite(value)) return '—'
  return new Intl.NumberFormat(locale, { style: 'percent', maximumFractionDigits: 0 }).format(
    Math.min(1, Math.max(0, value)),
  )
}

function durationLabel(value: number): string {
  if (value < 1000) return `${Math.round(value)} ms`
  return `${(value / 1000).toFixed(1)} s`
}

function evidenceItems(evidence: AssistantEvidenceView, t: TFunction): EvidenceItem[] {
  const segments = evidence.segmentRefs.map<EvidenceItem>((reference) => {
    const href = reference.sourceAccess?.href ?? reference.sourceUri
    return {
      ...(reference.excerpt ? { detail: reference.excerpt } : {}),
      documentId: reference.documentId,
      ...(href ? { href } : {}),
      id: `segment-${reference.documentId}-${reference.segmentOrdinal}`,
      rank: reference.segmentOrdinal,
      score: reference.relevance,
      ...(reference.sourceAccess ? { sourceKind: reference.sourceAccess.kind } : {}),
      title: reference.documentTitle || reference.documentName,
      type: 'segment',
    }
  })
  const chunks = (evidence.chunkRefs ?? []).map<EvidenceItem>((reference) => ({
    // A chunk used to render as "#3" with a six-figure internal score beside
    // it — a row a reader could neither read nor act on. Its own text, the
    // headings above it, and a way into the document are what make it a source.
    ...(reference.excerpt ? { detail: reference.excerpt } : {}),
    ...(reference.documentId ? { documentId: reference.documentId } : {}),
    id: `chunk-${reference.chunkId}`,
    rank: reference.rank,
    score: reference.score,
    ...(reference.sectionPath.length > 0 ? { sectionPath: reference.sectionPath.join(' / ') } : {}),
    title: reference.documentTitle || t('assistant.evidenceTypes.chunk'),
    type: 'chunk',
  }))
  const facts = evidence.factRefs.map<EvidenceItem>((reference, index) => ({
    detail: technicalFactKindLabel(reference.factKind, t),
    id: `fact-${reference.factKind}-${reference.value}-${index}`,
    rank: index + 1,
    score: reference.confidence,
    title: reference.value,
    type: 'fact',
  }))
  const entities = evidence.entityRefs.map<EvidenceItem>((reference, index) => ({
    entityType: reference.type,
    id: `entity-${reference.entityId}`,
    nodeId: reference.entityId,
    rank: index + 1,
    score: reference.relevance,
    title: reference.label,
    type: 'entity',
  }))
  const relations = evidence.relationRefs.map<EvidenceItem>((reference, index) => ({
    detail: `${reference.sourceLabel} → ${reference.targetLabel}`,
    id: `relation-${reference.relation}-${index}`,
    rank: index + 1,
    score: reference.weight,
    title: humanizeToken(reference.relation),
    type: 'relation',
  }))
  return [...segments, ...chunks, ...facts, ...entities, ...relations]
}

export function EvidenceDrawer({ evidence, open, onOpenChange }: EvidenceDrawerProps) {
  const { t } = useTranslation()
  const { locale } = useApp()
  const navigate = useNavigate()
  const [visibility, setVisibility] = useState({
    evidence,
    open,
    count: REFERENCES_PAGE_SIZE,
  })
  const visibleCount =
    visibility.evidence === evidence && visibility.open === open
      ? visibility.count
      : REFERENCES_PAGE_SIZE
  const view = evidence as AssistantEvidenceView | null
  const items = useMemo(() => (view ? evidenceItems(view, t) : []), [t, view])
  const total = Math.max(view?.referenceSummary?.totalCount ?? 0, items.length)
  const visibleItems = items.slice(0, visibleCount)

  const counts = view
    ? {
        chunk: view.chunkRefs?.length ?? 0,
        segment: view.runtimeSummary?.totalSegments ?? view.segmentRefs.length,
        fact: view.runtimeSummary?.totalFacts ?? view.factRefs.length,
        entity: view.runtimeSummary?.totalEntities ?? view.entityRefs.length,
        relation: view.runtimeSummary?.totalRelations ?? view.relationRefs.length,
      }
    : { chunk: 0, segment: 0, fact: 0, entity: 0, relation: 0 }

  return (
    <Drawer open={open} onOpenChange={onOpenChange} title={`${t('assistant.sources')} · ${total}`}>
      {items.length > 0 && (
        <div className="evidence-summary max-md:grid-cols-1!">
          {(Object.keys(counts) as Array<keyof typeof counts>).map((type) => (
            <div key={type}>
              <span>{t(`assistant.evidenceTypes.${type}`)}</span>
              <strong>{counts[type]}</strong>
            </div>
          ))}
        </div>
      )}

      {view?.runtimeSummary?.stages.length ? (
        <div className="debug-timeline">
          {view.runtimeSummary.stages.map((stage, index) => (
            <div key={`${stage.stage}-${index}`}>
              <FileText size={14} />
              <span>
                {t(`assistant.runtimeStageLabels.${stage.stage}`, {
                  defaultValue: humanizeToken(stage.stage),
                })}
              </span>
              <strong>{durationLabel(stage.durationMs)}</strong>
            </div>
          ))}
        </div>
      ) : null}

      <ScreenState
        scenario={items.length === 0 ? 'empty' : 'ready'}
        emptyText={t('common.empty')}
        emptyHint={t('assistant.noEvidenceDesc')}
      >
        <>
          <div className="evidence-list">
            {visibleItems.map((item) => (
              <article key={item.id}>
                <div className="evidence-rank">{item.rank}</div>
                <div>
                  <Badge>{t(`assistant.evidenceTypes.${item.type}`)}</Badge>
                  <strong>{item.title}</strong>
                  {item.sectionPath && <p className="evidence-path">{item.sectionPath}</p>}
                  {item.detail && <p>{item.detail}</p>}
                  {item.entityType && (
                    <p>
                      {t(`graph.nodeTypes.${item.entityType}`, {
                        defaultValue: humanizeToken(item.entityType),
                      })}
                    </p>
                  )}
                </div>
                {/* Only a normalised relevance is shown. The raw ranking score
                    runs into the millions — it orders the list correctly and
                    means nothing on its own, and printing it beside every row
                    read as a number the reader was supposed to judge. */}
                {item.score <= 1 && <span>{relevanceLabel(item.score, locale)}</span>}
                {item.documentId && !item.href && (
                  <button
                    type="button"
                    onClick={() => navigate(`/documents?documentId=${item.documentId}`)}
                  >
                    <FileText size={13} />
                    {t('assistant.openSourceDocument')}
                  </button>
                )}
                {item.nodeId && (
                  <button type="button" onClick={() => navigate(`/graph?nodeId=${item.nodeId}`)}>
                    <Network size={13} />
                    {t('assistant.openGraph')}
                  </button>
                )}
                {item.href && (
                  <a
                    href={item.href}
                    target={item.href.startsWith('http') ? '_blank' : undefined}
                    rel={item.href.startsWith('http') ? 'noreferrer' : undefined}
                  >
                    <ExternalLink size={13} />
                    {item.sourceKind === 'stored_document'
                      ? t('assistant.openSourceDocument')
                      : t('assistant.openSourceLink')}
                  </a>
                )}
              </article>
            ))}
          </div>
          {visibleCount < items.length && (
            <Button
              className="evidence-show-more"
              variant="ghost"
              onClick={() =>
                setVisibility({
                  evidence,
                  open,
                  count: visibleCount + REFERENCES_PAGE_SIZE,
                })
              }
            >
              {t('assistant.showMore')}
            </Button>
          )}
          <p className="evidence-truncated">
            {t('assistant.sourcesShown', {
              shown: Math.min(visibleItems.length, total),
              total,
            })}
          </p>
        </>
      </ScreenState>
    </Drawer>
  )
}
