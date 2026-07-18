import { memo, useMemo, useState, type ReactNode } from 'react'
import type { TFunction } from 'i18next'
import { useNavigate } from 'react-router-dom'
import {
  AlertTriangle,
  Clipboard,
  FileText,
  Info,
  Link2,
  Loader2,
  LocateFixed,
  MessageCircle,
  Network,
  Tags,
  X,
} from 'lucide-react'
import { toast } from 'sonner'
import { Button } from '@/shared/components/ui/button'
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from '@/shared/components/ui/tooltip'
import { compactText } from '@/shared/lib/compactText'
import { GRAPH_NODE_COLORS } from '@/features/graph/model/config'
import type { GraphNode } from '@/shared/types'
import type { GraphAdjacencyIndex } from '@/features/graph/hooks/useGraphAdjacency'

const SUMMARY_PREVIEW_CHARS = 260
const NEIGHBOR_FETCH_LIMIT = 32
const CONNECTED_ENTITIES_PREVIEW_LIMIT = 18
const CONNECTED_CONCEPTS_PREVIEW_LIMIT = 12
const SOURCE_DOCUMENTS_PREVIEW_LIMIT = 12

type GraphInspectorProps = Readonly<{
  t: TFunction
  /** The canonical selection to render (prefer `selectedDetail` with fallback to list node). */
  selected: GraphNode
  /** Still loading enriched detail from the backend — shows a spinner in the header. */
  detailLoading: boolean
  /** Shared adjacency index so the inspector resolves neighbors in O(k). */
  adjacency: GraphAdjacencyIndex
  detailError?: string
  onClose: () => void
  onSelectNode: (id: string) => void
  onFocusNeighborhood: (id: string) => void
}>

type InspectorSection = 'neighbors' | 'details' | 'sources'

type NeighborGroup = {
  docs: GraphNode[]
  entities: GraphNode[]
  concepts: GraphNode[]
  totalConnections: number
}

type IconComponent = (props: Readonly<{ className?: string }>) => ReactNode

const PROPERTY_LABEL_KEYS = new Map<string, string>([
  ['type', 'graph.propertyLabels.type'],
  ['confidence', 'graph.propertyLabels.confidence'],
  ['supportcount', 'graph.propertyLabels.supportCount'],
  ['state', 'graph.propertyLabels.state'],
  ['aliases', 'graph.propertyLabels.aliases'],
  ['format', 'graph.propertyLabels.format'],
  ['size', 'graph.propertyLabels.size'],
  ['revision', 'graph.propertyLabels.revision'],
  ['activity', 'graph.propertyLabels.activity'],
  ['graphcoverage', 'graph.propertyLabels.graphCoverage'],
])

function defaultSectionFor(selected: GraphNode): InspectorSection {
  return selected.type === 'document' ? 'details' : 'neighbors'
}

function groupNeighbors(selected: GraphNode, adjacency: GraphAdjacencyIndex): NeighborGroup {
  const neighborhood = adjacency.neighborhoodOf(selected.id, NEIGHBOR_FETCH_LIMIT)
  const docs: GraphNode[] = []
  const entities: GraphNode[] = []
  const concepts: GraphNode[] = []
  for (const node of neighborhood.nodes) {
    if (node.type === 'document') docs.push(node)
    else if (node.type === 'concept') concepts.push(node)
    else entities.push(node)
  }
  return { docs, entities, concepts, totalConnections: neighborhood.ids.length }
}

function propertyLabel(t: TFunction, key: string): string {
  const normalized = key
    .trim()
    .toLowerCase()
    .replace(/[\s_-]+/g, '')
  const translationKey = PROPERTY_LABEL_KEYS.get(normalized)
  return translationKey ? t(translationKey) : key
}

function structuredPropertyValue(value: object): string {
  try {
    return JSON.stringify(value) ?? ''
  } catch {
    return ''
  }
}

function primitivePropertyValue(value: unknown): string {
  if (value == null) return ''
  if (typeof value === 'string') return value
  if (typeof value === 'number' || typeof value === 'boolean' || typeof value === 'bigint') {
    return String(value)
  }
  if (typeof value === 'object') return structuredPropertyValue(value)
  return ''
}

function propertyValue(value: unknown): string {
  if (Array.isArray(value)) return value.map(primitivePropertyValue).join(', ')
  return primitivePropertyValue(value)
}

function InspectorMetric({
  icon: Icon,
  label,
  value,
}: Readonly<{
  icon: IconComponent
  label: string
  value: number
}>) {
  return (
    <div className="min-w-0 rounded-lg border border-border/60 bg-background/45 px-2.5 py-2">
      <div className="flex items-center gap-1.5 section-label">
        <Icon className="h-3.5 w-3.5 shrink-0" />
        <span className="truncate">{label}</span>
      </div>
      <div className="mt-1 truncate text-sm font-bold tabular-nums text-foreground">{value}</div>
    </div>
  )
}

function InspectorAction({
  children,
  label,
  onClick,
}: Readonly<{
  children: ReactNode
  label: string
  onClick: () => void
}>) {
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <Button
          type="button"
          variant="outline"
          size="sm"
          aria-label={label}
          title={label}
          className="h-8 w-8 shrink-0 rounded-lg p-0"
          onClick={onClick}
        >
          {children}
        </Button>
      </TooltipTrigger>
      <TooltipContent>{label}</TooltipContent>
    </Tooltip>
  )
}

function SectionButton({
  active,
  label,
  onClick,
}: Readonly<{
  active: boolean
  label: string
  onClick: () => void
}>) {
  return (
    <button
      type="button"
      aria-pressed={active}
      className={`min-w-0 flex-1 rounded-lg px-2.5 py-1.5 text-xs font-semibold transition-colors ${
        active
          ? 'bg-primary text-primary-foreground shadow-sm'
          : 'text-muted-foreground hover:bg-muted hover:text-foreground'
      }`}
      onClick={onClick}
    >
      <span className="block truncate">{label}</span>
    </button>
  )
}

function NeighborList({
  emptyLabel,
  limit,
  nodes,
  onSelectNode,
  title,
}: Readonly<{
  emptyLabel: string
  limit: number
  nodes: GraphNode[]
  onSelectNode: (id: string) => void
  title: string
}>) {
  const visibleNodes = nodes.slice(0, limit)
  return (
    <div>
      <div className="mb-1.5 flex items-center justify-between gap-2">
        <div className="section-label">{title}</div>
        <span className="text-2xs font-semibold tabular-nums text-muted-foreground">
          {nodes.length}
        </span>
      </div>
      {visibleNodes.length > 0 ? (
        <div className="space-y-0.5">
          {visibleNodes.map((node) => {
            const compactLabel = compactText(node.label, 52)
            return (
              <button
                key={node.id}
                type="button"
                className="group flex w-full items-center gap-2 rounded-lg px-2 py-1.5 text-left text-xs transition-colors hover:bg-accent/60"
                onClick={() => onSelectNode(node.id)}
              >
                <span
                  className="h-2 w-2 shrink-0 rounded-full"
                  style={{ background: GRAPH_NODE_COLORS[node.type] ?? GRAPH_NODE_COLORS.entity }}
                />
                <span
                  className="min-w-0 flex-1 truncate font-medium text-foreground"
                  title={compactLabel.fullText}
                >
                  {compactLabel.text}
                </span>
                {node.edgeCount > 0 && (
                  <span className="shrink-0 text-2xs tabular-nums text-muted-foreground">
                    {node.edgeCount}
                  </span>
                )}
              </button>
            )
          })}
          {nodes.length > limit && (
            <div className="pl-4 text-xs text-muted-foreground">{emptyLabel}</div>
          )}
        </div>
      ) : (
        <p className="text-xs text-muted-foreground">{emptyLabel}</p>
      )}
    </div>
  )
}

function GraphInspectorImpl({
  t,
  selected,
  detailLoading,
  adjacency,
  detailError,
  onClose,
  onSelectNode,
  onFocusNeighborhood,
}: GraphInspectorProps) {
  const navigate = useNavigate()
  const [summaryState, setSummaryState] = useState({ nodeId: selected.id, expanded: false })
  const [sectionState, setSectionState] = useState<{
    nodeId: string
    section: InspectorSection
  }>({ nodeId: selected.id, section: defaultSectionFor(selected) })

  // Re-group neighbors only when the selected node or the adjacency index changes.
  // Typing in the search box or expanding the summary no longer triggers this walk.
  const neighbors = useMemo<NeighborGroup>(
    () => groupNeighbors(selected, adjacency),
    [selected, adjacency],
  )

  const summaryExpanded = summaryState.nodeId === selected.id ? summaryState.expanded : false
  const activeSection =
    sectionState.nodeId === selected.id ? sectionState.section : defaultSectionFor(selected)
  const setActiveSection = (section: InspectorSection) => {
    setSectionState({ nodeId: selected.id, section })
  }

  const summary = selected.summary?.trim() ?? ''
  const isLongSummary = summary.length > SUMMARY_PREVIEW_CHARS
  const visibleSummary =
    !isLongSummary || summaryExpanded
      ? summary
      : `${summary.slice(0, SUMMARY_PREVIEW_CHARS).trimEnd()}…`

  const propertyEntries = useMemo(
    () =>
      Object.entries(selected.properties).map(
        ([key, value]) => [key, propertyValue(value)] as const,
      ),
    [selected.properties],
  )

  const copyNodeLink = () => {
    if (typeof window === 'undefined') return
    const url = new URL(window.location.href)
    url.searchParams.set('nodeId', selected.id)
    void navigator.clipboard
      .writeText(url.toString())
      .then(() => toast.success(t('graph.nodeLinkCopied')))
      .catch(() => toast.error(t('graph.clipboardUnavailable')))
  }

  const copyLabel = () => {
    void navigator.clipboard
      .writeText(selected.label)
      .then(() => toast.success(t('graph.labelCopied')))
      .catch(() => toast.error(t('graph.clipboardUnavailable')))
  }

  const askAboutSelected = () => {
    navigator.clipboard
      .writeText(selected.label)
      .then(() => {
        toast.info(t('graph.askAboutThisCopied'))
        return navigate('/assistant')
      })
      .catch(() => toast.error(t('graph.clipboardUnavailable')))
  }

  const moreEntitiesLabel = t('common.moreCount', {
    count: Math.max(0, neighbors.entities.length - CONNECTED_ENTITIES_PREVIEW_LIMIT),
  })
  const moreConceptsLabel = t('common.moreCount', {
    count: Math.max(0, neighbors.concepts.length - CONNECTED_CONCEPTS_PREVIEW_LIMIT),
  })
  const moreDocumentsLabel = t('common.moreCount', {
    count: Math.max(0, neighbors.docs.length - SOURCE_DOCUMENTS_PREVIEW_LIMIT),
  })

  return (
    <TooltipProvider delayDuration={180}>
      <div className="absolute inset-x-2 bottom-2 top-auto z-30 flex max-h-[76dvh] flex-col overflow-hidden rounded-xl border border-border/70 bg-popover/95 shadow-2xl backdrop-blur-md [contain:layout_paint_style] md:inset-y-3 md:left-auto md:right-3 md:max-h-none md:w-[24rem] lg:w-[28rem] xl:w-[30rem]">
        <div
          className="absolute inset-y-0 left-0 w-1"
          style={{ background: GRAPH_NODE_COLORS[selected.type] ?? GRAPH_NODE_COLORS.entity }}
        />
        <div className="border-b border-border/70 px-4 py-3 pl-5">
          <div className="flex items-start gap-2">
            <div className="min-w-0 flex-1">
              <div className="mb-1 flex items-center gap-2">
                <span
                  className="h-2.5 w-2.5 shrink-0 rounded-full"
                  style={{
                    background: GRAPH_NODE_COLORS[selected.type] ?? GRAPH_NODE_COLORS.entity,
                  }}
                />
                <span className="truncate section-label">
                  {t(`graph.nodeTypes.${selected.type}`)}
                  {selected.subType ? ` · ${selected.subType}` : ''}
                </span>
              </div>
              <h3
                className="text-[15px] font-bold leading-5 tracking-tight text-foreground [overflow-wrap:anywhere]"
                title={selected.label}
              >
                {selected.label}
              </h3>
            </div>
            <div className="flex shrink-0 items-center gap-1">
              {detailLoading && (
                <Loader2 className="h-3.5 w-3.5 animate-spin text-muted-foreground" />
              )}
              <Button
                type="button"
                variant="ghost"
                size="icon"
                className="h-8 w-8"
                onClick={onClose}
                aria-label={t('common.close')}
                title={t('common.close')}
              >
                <X className="h-4 w-4" />
              </Button>
            </div>
          </div>
        </div>

        <div className="grid grid-cols-3 gap-1.5 border-b border-border/70 px-4 py-2.5 pl-5">
          <InspectorMetric
            icon={Network}
            label={t('graph.connections')}
            value={neighbors.totalConnections}
          />
          <InspectorMetric
            icon={FileText}
            label={t('graph.sources')}
            value={neighbors.docs.length}
          />
          <InspectorMetric
            icon={Tags}
            label={t('graph.properties')}
            value={propertyEntries.length}
          />
        </div>

        {detailError && (
          <div className="mx-4 mt-3 flex items-start gap-2 rounded-lg border border-destructive/30 bg-destructive/10 p-3 text-xs leading-relaxed text-destructive">
            <AlertTriangle className="mt-0.5 h-3.5 w-3.5 shrink-0" />
            <span>{detailError}</span>
          </div>
        )}

        <div className="flex items-center gap-1.5 border-b border-border/70 px-4 py-2.5 pl-5">
          <InspectorAction label={t('graph.copyLabel')} onClick={copyLabel}>
            <Clipboard className="h-3.5 w-3.5" />
          </InspectorAction>
          <InspectorAction label={t('graph.copyNodeLink')} onClick={copyNodeLink}>
            <Link2 className="h-3.5 w-3.5" />
          </InspectorAction>
          <InspectorAction
            label={t('graph.focusNeighborhood')}
            onClick={() => onFocusNeighborhood(selected.id)}
          >
            <LocateFixed className="h-3.5 w-3.5" />
          </InspectorAction>
          {(selected.type === 'entity' || selected.type === 'concept') && (
            <InspectorAction label={t('graph.askAboutThis')} onClick={askAboutSelected}>
              <MessageCircle className="h-3.5 w-3.5" />
            </InspectorAction>
          )}
          {selected.type === 'document' && (
            <Button
              variant="outline"
              size="sm"
              className="ml-auto h-8 min-w-0 rounded-lg px-2.5 text-xs font-semibold"
              onClick={() => navigate(`/documents?documentId=${encodeURIComponent(selected.id)}`)}
            >
              <FileText className="mr-1.5 h-3.5 w-3.5 shrink-0" />
              <span className="truncate">{t('graph.viewDocument')}</span>
            </Button>
          )}
        </div>

        <div className="border-b border-border/70 px-4 py-2 pl-5">
          <div className="flex rounded-xl bg-muted/55 p-1">
            <SectionButton
              active={activeSection === 'neighbors'}
              label={t('graph.neighbors')}
              onClick={() => setActiveSection('neighbors')}
            />
            <SectionButton
              active={activeSection === 'details'}
              label={t('graph.details')}
              onClick={() => setActiveSection('details')}
            />
            <SectionButton
              active={activeSection === 'sources'}
              label={t('graph.sources')}
              onClick={() => setActiveSection('sources')}
            />
          </div>
        </div>

        <div className="min-h-0 flex-1 overflow-y-auto px-4 py-3 pl-5">
          {activeSection === 'neighbors' && (
            <div className="space-y-4">
              {neighbors.docs.length + neighbors.entities.length + neighbors.concepts.length ===
                0 && !detailLoading ? (
                <p className="text-xs text-muted-foreground">{t('graph.noConnections')}</p>
              ) : (
                <>
                  {neighbors.docs.length > 0 && (
                    <NeighborList
                      title={t('graph.sourceDocuments')}
                      nodes={neighbors.docs}
                      limit={SOURCE_DOCUMENTS_PREVIEW_LIMIT}
                      emptyLabel={moreDocumentsLabel}
                      onSelectNode={onSelectNode}
                    />
                  )}
                  {neighbors.entities.length > 0 && (
                    <NeighborList
                      title={t('graph.connectedEntities')}
                      nodes={neighbors.entities}
                      limit={CONNECTED_ENTITIES_PREVIEW_LIMIT}
                      emptyLabel={moreEntitiesLabel}
                      onSelectNode={onSelectNode}
                    />
                  )}
                  {neighbors.concepts.length > 0 && (
                    <NeighborList
                      title={t('graph.connectedConcepts')}
                      nodes={neighbors.concepts}
                      limit={CONNECTED_CONCEPTS_PREVIEW_LIMIT}
                      emptyLabel={moreConceptsLabel}
                      onSelectNode={onSelectNode}
                    />
                  )}
                </>
              )}
            </div>
          )}

          {activeSection === 'details' && (
            <div className="space-y-4">
              {summary && (
                <div>
                  <div className="mb-1 flex items-center gap-1.5">
                    <Info className="h-3.5 w-3.5 text-muted-foreground" />
                    <div className="section-label">{t('graph.summary')}</div>
                  </div>
                  <p className="whitespace-pre-wrap text-sm leading-relaxed text-muted-foreground [overflow-wrap:anywhere]">
                    {visibleSummary}
                  </p>
                  {isLongSummary && (
                    <button
                      type="button"
                      onClick={() =>
                        setSummaryState({ nodeId: selected.id, expanded: !summaryExpanded })
                      }
                      className="mt-1 text-xs font-semibold text-primary hover:underline"
                    >
                      {summaryExpanded ? t('graph.summaryCollapse') : t('graph.summaryExpand')}
                    </button>
                  )}
                </div>
              )}

              {selected.type !== 'document' && (
                <div className="grid grid-cols-[minmax(0,8rem)_minmax(0,1fr)] items-start gap-x-3 text-xs">
                  <span className="text-muted-foreground">{t('graph.subType')}</span>
                  <span className="min-w-0 text-right font-semibold text-foreground [overflow-wrap:anywhere]">
                    {selected.subType ?? '—'}
                  </span>
                </div>
              )}

              {propertyEntries.length > 0 && (
                <div>
                  <div className="section-label mb-1.5">{t('graph.properties')}</div>
                  <div className="space-y-1">
                    {propertyEntries.map(([key, value]) => (
                      <div
                        key={key}
                        className="grid grid-cols-[minmax(0,8rem)_minmax(0,1fr)] items-start gap-x-3 rounded-md py-0.5 text-xs"
                      >
                        <span className="min-w-0 truncate text-muted-foreground" title={key}>
                          {propertyLabel(t, key)}
                        </span>
                        <span className="min-w-0 text-right font-semibold leading-tight text-foreground [overflow-wrap:anywhere]">
                          {value}
                        </span>
                      </div>
                    ))}
                  </div>
                </div>
              )}
            </div>
          )}

          {activeSection === 'sources' && (
            <NeighborList
              title={t('graph.sourceDocuments')}
              nodes={neighbors.docs}
              limit={SOURCE_DOCUMENTS_PREVIEW_LIMIT}
              emptyLabel={
                neighbors.docs.length > SOURCE_DOCUMENTS_PREVIEW_LIMIT
                  ? moreDocumentsLabel
                  : t('graph.noSources')
              }
              onSelectNode={onSelectNode}
            />
          )}
        </div>
      </div>
    </TooltipProvider>
  )
}

export const GraphInspector = memo(GraphInspectorImpl)
