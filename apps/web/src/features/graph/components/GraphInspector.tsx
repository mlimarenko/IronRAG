import { useQuery } from '@tanstack/react-query'
import { Copy, FileText, Focus, MessageCircle, X } from 'lucide-react'
import { useMemo, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useNavigate } from 'react-router-dom'
import { queries } from '@/shared/api'
import { entityStateLabel, humanizeToken } from '@/shared/lib/enumLabels'
import type { GraphNode } from '@/shared/types'
import { Badge, Button, IconButton } from '@/shared/ui'
import { toast } from 'sonner'
import type { GraphAdjacencyIndex } from '../hooks/useGraphAdjacency'
import { GRAPH_NODE_COLORS } from '../model/config'
import { mapGraphDocumentDetail, mapKnowledgeEntityDetail } from '../model/graphAdapter'

type GraphInspectorProps = Readonly<{
  libraryId: string
  selectedBasic: GraphNode | null
  adjacency: GraphAdjacencyIndex
  neighborhoodFocused: boolean
  onClose: () => void
  onSelectNode: (id: string) => void
  onFocusNeighborhood: (id: string) => void
}>

function nodeLink(nodeId: string): string {
  if (typeof window === 'undefined') return `?nodeId=${encodeURIComponent(nodeId)}`
  const url = new URL(window.location.href)
  url.searchParams.set('nodeId', nodeId)
  return url.toString()
}

export function GraphInspector({
  libraryId,
  selectedBasic,
  adjacency,
  neighborhoodFocused,
  onClose,
  onSelectNode,
  onFocusNeighborhood,
}: GraphInspectorProps) {
  const { t } = useTranslation()
  const navigate = useNavigate()
  const selectedId = selectedBasic?.id ?? ''
  const isDocument = selectedBasic?.type === 'document'
  const [neighborLimit, setNeighborLimit] = useState({ nodeId: selectedId, value: 32 })
  const visibleNeighbors = neighborLimit.nodeId === selectedId ? neighborLimit.value : 32

  const entityQuery = useQuery({
    ...queries.getKnowledgeEntityOptions({
      path: { libraryId, entityId: selectedId },
    }),
    enabled: Boolean(selectedBasic) && !isDocument,
    select: (detail) => mapKnowledgeEntityDetail(detail, selectedBasic, selectedId),
  })
  const documentQuery = useQuery({
    ...queries.getContentDocumentOptions({
      path: { documentId: selectedId },
    }),
    enabled: Boolean(selectedBasic) && isDocument,
    retry: false,
    select: (detail) => mapGraphDocumentDetail(detail, selectedBasic, selectedId),
  })

  const selected = (isDocument ? documentQuery.data : entityQuery.data) ?? selectedBasic
  const detailLoading = isDocument ? documentQuery.isLoading : entityQuery.isLoading
  const detailError = isDocument ? documentQuery.isError : entityQuery.isError
  const neighborhood = useMemo(
    () => adjacency.neighborhoodOf(selected?.id ?? null, visibleNeighbors),
    [adjacency, selected?.id, visibleNeighbors],
  )
  const totalNeighbors = selected ? (adjacency.neighborIds.get(selected.id)?.length ?? 0) : 0
  const aliases =
    selected?.properties.aliases
      ?.split(',')
      .map((alias) => alias.trim())
      .filter(Boolean) ?? []
  const sourceDocumentIds = selected?.sourceDocumentIds ?? []

  const openDocument = (documentId: string) => {
    void navigate(`/documents?documentId=${encodeURIComponent(documentId)}`)
  }

  const askAboutSelected = () => {
    if (!selected) return
    const typeLabel = t(`graph.nodeTypes.${selected.type}`)
    const prompt = t('graph.askAboutNodePrompt', { label: selected.label, type: typeLabel })
    void navigate(`/assistant?prompt=${encodeURIComponent(prompt)}`)
  }

  const copyNodeLink = () => {
    if (!selected) return
    void navigator.clipboard.writeText(nodeLink(selected.id))
    toast.success(t('common.copied'))
  }

  if (!selectedBasic || !selected) return null

  return (
    <aside className="graph-inspector" aria-label={t('graph.nodeInspector')}>
      <header className="graph-inspector__chrome">
        <div className="graph-inspector__identity">
          <span
            className="node-swatch"
            style={{
              backgroundColor: GRAPH_NODE_COLORS[selected.type] ?? GRAPH_NODE_COLORS.entity,
            }}
          />
          <Badge>{t(`graph.nodeTypes.${selected.type}`)}</Badge>
          {selected.subType && <small>{humanizeToken(selected.subType)}</small>}
        </div>
        <div className="graph-inspector__chrome-actions">
          <IconButton label={t('graph.copyNodeLink')} onClick={copyNodeLink}>
            <Copy size={16} />
          </IconButton>
          <IconButton label={t('common.close')} onClick={onClose}>
            <X size={16} />
          </IconButton>
        </div>
      </header>

      <h2 className="graph-inspector__title">{selected.label}</h2>
      <div className="graph-inspector__actions">
        <Button size="sm" variant="primary" onClick={askAboutSelected}>
          <MessageCircle size={14} />
          {t('graph.askAboutThis')}
        </Button>
        {isDocument && (
          <Button size="sm" onClick={() => openDocument(selected.id)}>
            <FileText size={14} />
            {t('graph.openDocument')}
          </Button>
        )}
      </div>
      {detailLoading && <p className="node-detail-state">{t('common.loading')}</p>}
      {detailError && (
        <p className="node-detail-state node-detail-state--error" role="alert">
          {t('graph.detailsFailed')}
        </p>
      )}
      {selected.summary && <p className="node-summary">{selected.summary}</p>}

      <div className="node-metrics">
        <div>
          <span>{t('graph.connections')}</span>
          <strong>{totalNeighbors}</strong>
        </div>
        {selected.properties['support count'] && (
          <div>
            <span>{t('graph.support')}</span>
            <strong>{selected.properties['support count']}</strong>
          </div>
        )}
        {selected.properties.state && (
          <div>
            <span>{t('common.status')}</span>
            <strong>{entityStateLabel(selected.properties.state, t)}</strong>
          </div>
        )}
      </div>

      {aliases.length > 0 && (
        <section>
          <h3>{t('graph.aliases')}</h3>
          <div className="chip-row">
            {aliases.map((alias) => (
              <Badge key={alias}>{alias}</Badge>
            ))}
          </div>
        </section>
      )}

      <section>
        <div className="node-section-heading">
          <h3>{t('graph.neighbors')}</h3>
          <div className="graph-inspector__neighbor-tools">
            {totalNeighbors > 0 && (
              <span>
                {Math.min(neighborhood.nodes.length, totalNeighbors)} {t('graph.of')}{' '}
                {totalNeighbors}
              </span>
            )}
            {totalNeighbors > 0 && (
              <Button
                size="sm"
                variant="secondary"
                aria-pressed={neighborhoodFocused}
                onClick={() => onFocusNeighborhood(selected.id)}
              >
                <Focus size={14} />
                {neighborhoodFocused ? t('graph.showFullGraph') : t('graph.showOnlyNeighborhood')}
              </Button>
            )}
          </div>
        </div>
        {neighborhood.nodes.length > 0 ? (
          <>
            {neighborhood.nodes.map((node) => (
              <button
                className="neighbor-row"
                type="button"
                key={node.id}
                onClick={() => onSelectNode(node.id)}
              >
                <span
                  className="node-swatch"
                  style={{
                    backgroundColor: GRAPH_NODE_COLORS[node.type] ?? GRAPH_NODE_COLORS.entity,
                  }}
                />
                <span>{node.label}</span>
                <Badge>{t(`graph.nodeTypes.${node.type}`)}</Badge>
              </button>
            ))}
            {neighborhood.nodes.length < totalNeighbors && (
              <Button
                className="node-show-more"
                size="sm"
                variant="ghost"
                onClick={() =>
                  setNeighborLimit({ nodeId: selectedId, value: visibleNeighbors + 32 })
                }
              >
                {t('graph.showMore')}
              </Button>
            )}
          </>
        ) : (
          <p className="node-section-empty">{t('graph.noConnections')}</p>
        )}
      </section>

      {selected.type !== 'document' && (
        <section>
          <h3>{t('graph.sources')}</h3>
          {sourceDocumentIds.length > 0 ? (
            sourceDocumentIds.map((documentId) => (
              <button
                className="source-document"
                type="button"
                key={documentId}
                onClick={() => openDocument(documentId)}
              >
                <FileText size={15} />
                <span>
                  {adjacency.nodeById.get(documentId)?.label ?? t('graph.documentFallback')}
                </span>
              </button>
            ))
          ) : (
            <p className="node-section-empty">{t('graph.noSources')}</p>
          )}
        </section>
      )}
    </aside>
  )
}
