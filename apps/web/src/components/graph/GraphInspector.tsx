import { memo, useMemo, useState } from 'react';
import type { TFunction } from 'i18next';
import { useNavigate } from 'react-router-dom';
import { FileText, Loader2, X } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { compactText } from '@/lib/compactText';
import { GRAPH_NODE_COLORS } from '@/components/graph/config';
import type { GraphNode } from '@/types';
import type { GraphAdjacencyIndex } from './useGraphAdjacency';

const SUMMARY_PREVIEW_CHARS = 280;
const NEIGHBOR_FETCH_LIMIT = 20;
const CONNECTED_ENTITIES_PREVIEW_LIMIT = 15;
const CONNECTED_CONCEPTS_PREVIEW_LIMIT = 10;

type GraphInspectorProps = {
  t: TFunction;
  /** The canonical selection to render (prefer `selectedDetail` with fallback to list node). */
  selected: GraphNode;
  /** Still loading enriched detail from the backend — shows a spinner in the header. */
  detailLoading: boolean;
  /** Shared adjacency index so the inspector resolves neighbors in O(k). */
  adjacency: GraphAdjacencyIndex;
  onClose: () => void;
  onSelectNode: (id: string) => void;
};

type NeighborGroup = {
  docs: GraphNode[];
  entities: GraphNode[];
  concepts: GraphNode[];
  totalConnections: number;
};

function groupNeighbors(selected: GraphNode, adjacency: GraphAdjacencyIndex): NeighborGroup {
  const neighborhood = adjacency.neighborhoodOf(selected.id, NEIGHBOR_FETCH_LIMIT);
  const docs: GraphNode[] = [];
  const entities: GraphNode[] = [];
  const concepts: GraphNode[] = [];
  for (const node of neighborhood.nodes) {
    if (node.type === 'document') docs.push(node);
    else if (node.type === 'entity') entities.push(node);
    else if (node.type === 'concept') concepts.push(node);
  }
  return { docs, entities, concepts, totalConnections: neighborhood.ids.length };
}

function GraphInspectorImpl({
  t,
  selected,
  detailLoading,
  adjacency,
  onClose,
  onSelectNode,
}: GraphInspectorProps) {
  const navigate = useNavigate();
  const [summaryExpanded, setSummaryExpanded] = useState(false);

  // Re-group neighbors only when the selected node or the adjacency index changes.
  // Typing in the search box or expanding the summary no longer triggers this walk.
  const neighbors = useMemo<NeighborGroup>(
    () => groupNeighbors(selected, adjacency),
    [selected, adjacency],
  );

  // Reset the summary-expand toggle when the user selects a different node so
  // each fresh selection starts collapsed.
  const summary = selected.summary?.trim() ?? '';
  const isLongSummary = summary.length > SUMMARY_PREVIEW_CHARS;
  const visibleSummary =
    !isLongSummary || summaryExpanded
      ? summary
      : `${summary.slice(0, SUMMARY_PREVIEW_CHARS).trimEnd()}…`;

  const propertyEntries = useMemo(() => Object.entries(selected.properties), [selected.properties]);

  return (
    <div className="absolute top-0 right-0 z-20 h-full w-[24rem] overflow-y-auto border-l bg-card shadow-xl animate-slide-in-right lg:w-[30rem] xl:w-[34rem]">
      <div className="flex items-start gap-2 border-b p-4">
        <h3
          className="min-w-0 flex-1 text-[15px] font-bold tracking-tight leading-5 text-foreground [overflow-wrap:anywhere]"
          title={selected.label}
        >
          {selected.label}
        </h3>
        <div className="flex items-center gap-1">
          {detailLoading && <Loader2 className="h-3.5 w-3.5 animate-spin text-muted-foreground" />}
          <button
            onClick={onClose}
            className="p-1.5 rounded-lg hover:bg-muted transition-colors"
            aria-label={t('common.close')}
          >
            <X className="h-4 w-4" />
          </button>
        </div>
      </div>
      <div className="p-4 space-y-4">
        <div className="flex items-center gap-2.5">
          <span
            className="w-3 h-3 rounded-full"
            style={{ background: GRAPH_NODE_COLORS[selected.type] }}
          />
          <div className="flex flex-col">
            <span className="text-sm font-semibold capitalize">
              {t(`graph.nodeTypes.${selected.type}`)}
            </span>
            {selected.subType && (
              <span className="text-[11px] text-muted-foreground capitalize">{selected.subType}</span>
            )}
          </div>
          <span className="text-xs text-muted-foreground ml-auto tabular-nums font-medium">
            {neighbors.totalConnections} {t('graph.connections')}
          </span>
        </div>

        {selected.type !== 'document' && (
          <div className="flex items-center justify-between text-xs">
            <span className="text-muted-foreground">{t('graph.subType')}</span>
            <span className="font-medium text-foreground">{selected.subType ?? '—'}</span>
          </div>
        )}

        {summary && (
          <div>
            <div className="section-label mb-1">{t('graph.summary')}</div>
            <p className="text-sm leading-relaxed text-muted-foreground whitespace-pre-wrap [overflow-wrap:anywhere]">
              {visibleSummary}
            </p>
            {isLongSummary && (
              <button
                type="button"
                onClick={() => setSummaryExpanded((prev) => !prev)}
                className="mt-1 text-xs font-semibold text-primary hover:underline"
              >
                {summaryExpanded ? t('graph.summaryCollapse') : t('graph.summaryExpand')}
              </button>
            )}
          </div>
        )}

        {propertyEntries.length > 0 && (
          <div>
            <div className="section-label mb-1.5">{t('graph.properties')}</div>
            <div className="space-y-1">
              {propertyEntries.map(([k, v]) => (
                <div
                  key={k}
                  className="grid grid-cols-[80px_minmax(0,1fr)] items-start gap-x-3 text-xs"
                >
                  <span className="pt-0.5 text-muted-foreground capitalize">{k}</span>
                  <span className="min-w-0 text-right font-semibold leading-tight text-foreground [overflow-wrap:anywhere]">
                    {v}
                  </span>
                </div>
              ))}
            </div>
          </div>
        )}

        <div className="flex gap-2">
          {selected.type === 'document' && (
            <Button
              variant="outline"
              size="sm"
              className="text-xs h-7"
              onClick={() =>
                navigate(`/documents?documentId=${encodeURIComponent(selected.id)}`)
              }
            >
              <FileText className="h-3 w-3 mr-1" /> {t('graph.viewDocument')}
            </Button>
          )}
        </div>

        {neighbors.docs.length > 0 && (
          <div>
            <div className="section-label mb-1.5">
              {t('graph.sourceDocuments')} ({neighbors.docs.length})
            </div>
            <div className="space-y-0.5">
              {neighbors.docs.map((n) => {
                const compactLabel = compactText(n.label, 48);
                return (
                  <button
                    key={n.id}
                    className="w-full flex items-center gap-2 p-2 rounded-lg hover:bg-accent/50 text-left text-xs transition-colors"
                    onClick={() => onSelectNode(n.id)}
                  >
                    <span
                      className="w-2 h-2 rounded-full shrink-0"
                      style={{ background: GRAPH_NODE_COLORS.document }}
                    />
                    <span className="truncate font-medium" title={compactLabel.fullText}>
                      {compactLabel.text}
                    </span>
                  </button>
                );
              })}
            </div>
          </div>
        )}

        {neighbors.entities.length > 0 && (
          <div>
            <div className="section-label mb-1.5">
              {t('graph.connectedEntities')} ({neighbors.entities.length})
            </div>
            <div className="space-y-0.5">
              {neighbors.entities.slice(0, CONNECTED_ENTITIES_PREVIEW_LIMIT).map((n) => {
                const compactLabel = compactText(n.label, 48);
                return (
                  <button
                    key={n.id}
                    className="w-full flex items-center gap-2 p-2 rounded-lg hover:bg-accent/50 text-left text-xs transition-colors"
                    onClick={() => onSelectNode(n.id)}
                  >
                    <span
                      className="w-2 h-2 rounded-full shrink-0"
                      style={{ background: GRAPH_NODE_COLORS.entity }}
                    />
                    <span className="truncate" title={compactLabel.fullText}>
                      {compactLabel.text}
                    </span>
                    {n.edgeCount > 0 && (
                      <span className="text-[10px] text-muted-foreground ml-auto tabular-nums">
                        {n.edgeCount}
                      </span>
                    )}
                  </button>
                );
              })}
              {neighbors.entities.length > CONNECTED_ENTITIES_PREVIEW_LIMIT && (
                <span className="text-xs text-muted-foreground pl-6">
                  +{neighbors.entities.length - CONNECTED_ENTITIES_PREVIEW_LIMIT} more
                </span>
              )}
            </div>
          </div>
        )}

        {neighbors.concepts.length > 0 && (
          <div>
            <div className="section-label mb-1.5">
              {t('graph.connectedConcepts')} ({neighbors.concepts.length})
            </div>
            <div className="space-y-0.5">
              {neighbors.concepts.slice(0, CONNECTED_CONCEPTS_PREVIEW_LIMIT).map((n) => {
                const compactLabel = compactText(n.label, 48);
                return (
                  <button
                    key={n.id}
                    className="w-full flex items-center gap-2 p-2 rounded-lg hover:bg-accent/50 text-left text-xs transition-colors"
                    onClick={() => onSelectNode(n.id)}
                  >
                    <span
                      className="w-2 h-2 rounded-full shrink-0"
                      style={{ background: GRAPH_NODE_COLORS.concept }}
                    />
                    <span className="truncate" title={compactLabel.fullText}>
                      {compactLabel.text}
                    </span>
                  </button>
                );
              })}
            </div>
          </div>
        )}

        {neighbors.docs.length + neighbors.entities.length + neighbors.concepts.length === 0 &&
          !detailLoading && (
            <p className="text-xs text-muted-foreground">{t('graph.noConnections')}</p>
          )}
      </div>
    </div>
  );
}

export const GraphInspector = memo(GraphInspectorImpl);
