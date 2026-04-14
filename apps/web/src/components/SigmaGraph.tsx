import { useEffect, useMemo, useRef, useState } from 'react';
import Graph from 'graphology';
import Sigma from 'sigma';
import { EdgeCurvedArrowProgram } from '@sigma/edge-curve';
import type { GraphNode } from '@/types';
import {
  buildGraphCanvasLabel,
  buildGraphFocusLabel,
  GRAPH_EDGE_COLORS,
  GRAPH_NODE_COLORS,
  selectProminentGraphLabelIds,
  type GraphLayoutType,
} from '@/components/graph/config';
import { applyGraphLayout } from '@/components/graph/layouts';

interface EdgeData {
  id: string;
  sourceId: string;
  targetId: string;
  label: string;
  weight: number;
}

interface SigmaGraphProps {
  nodes: GraphNode[];
  edges: EdgeData[];
  selectedId: string | null;
  onSelect: (id: string | null) => void;
  layout: GraphLayoutType;
}

const LAYOUT_ANIMATION_DURATION_MS = 280;
function cloneGraphStructure(source: Graph): Graph {
  const cloned = new Graph();

  source.forEachNode((node, attrs) => {
    cloned.addNode(node, { ...attrs });
  });

  source.forEachEdge((edge, attrs, sourceId, targetId) => {
    cloned.addEdgeWithKey(edge, sourceId, targetId, { ...attrs });
  });

  return cloned;
}

// --- Component ---

export default function SigmaGraph({ nodes, edges, selectedId, onSelect, layout }: SigmaGraphProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const tooltipRef = useRef<HTMLDivElement>(null);
  const sigmaRef = useRef<Sigma | null>(null);
  const graphRef = useRef<Graph | null>(null);
  const dragStateRef = useRef<{ dragging: boolean; node: string | null }>({ dragging: false, node: null });
  const [hoveredId, setHoveredId] = useState<string | null>(null);
  const layoutRef = useRef(layout);
  const layoutAnimationFrameRef = useRef<number | null>(null);
  const layoutAnimationTokenRef = useRef(0);
  // Pre-computed `nodeId -> Set<neighborId>` lookup, rebuilt once per
  // (nodes, edges) change. The hover/click reducer used to call
  // `graph.neighbors(id)` on every effect run, which on a 25k-node graph
  // walks the full adjacency list each time. With a precomputed Map,
  // hover lookup becomes O(1). Built via useMemo so it only recomputes
  // when the input arrays actually change.
  const neighborIndex = useMemo(() => {
    const index = new Map<string, Set<string>>();
    for (const edge of edges) {
      if (edge.sourceId === edge.targetId) continue;
      let outSet = index.get(edge.sourceId);
      if (!outSet) {
        outSet = new Set();
        index.set(edge.sourceId, outSet);
      }
      outSet.add(edge.targetId);
      let inSet = index.get(edge.targetId);
      if (!inSet) {
        inSet = new Set();
        index.set(edge.targetId, inSet);
      }
      inSet.add(edge.sourceId);
    }
    return index;
  }, [nodes, edges]);

  // Cheap `nodeId -> label` lookup so the DOM tooltip can resolve names
  // without touching the Sigma graph instance. Built once per `nodes`
  // change, O(N) memory.
  const labelByNodeId = useMemo(() => {
    const map = new Map<string, string>();
    for (const n of nodes) map.set(n.id, n.label);
    return map;
  }, [nodes]);

  // DOM-only tooltip state. The card is anchored to the node's viewport
  // position (via `sigma.graphToViewport`), not to the cursor — so it
  // stays attached to the right node and never leaves a "tail" behind
  // when the cursor moves away. Position recomputed on hover commit and
  // on each Sigma camera update.
  const [tooltip, setTooltip] = useState<{
    nodeId: string;
    label: string;
    neighborLabels: string[];
    neighborCount: number;
  } | null>(null);
  const [tooltipPos, setTooltipPos] = useState<{ x: number; y: number } | null>(null);
  // **Dwell-time hover**. The hover state only commits after the cursor
  // has been on the same node for `HOVER_DWELL_MS`. Fast sweeps across a
  // dense graph never commit, so they cost nothing — we never run the
  // expensive Sigma reducer + refresh path until the user actually
  // *stops* to look at a node. Tooltip + card show immediately though,
  // independent of dwell, since they live outside Sigma.
  const HOVER_DWELL_MS = 140;
  const pendingHoverRef = useRef<string | null>(null);
  const hoverTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const scheduleHoverUpdate = (next: string | null) => {
    pendingHoverRef.current = next;
    if (hoverTimerRef.current != null) {
      clearTimeout(hoverTimerRef.current);
      hoverTimerRef.current = null;
    }
    // Clearing hover (leaveNode) is immediate: no dwell wait.
    if (next == null) {
      setHoveredId((current) => (current == null ? current : null));
      return;
    }
    hoverTimerRef.current = setTimeout(() => {
      hoverTimerRef.current = null;
      setHoveredId((current) =>
        current === pendingHoverRef.current ? current : pendingHoverRef.current,
      );
    }, HOVER_DWELL_MS);
  };

  const stopLayoutAnimation = () => {
    layoutAnimationTokenRef.current += 1;
    if (layoutAnimationFrameRef.current != null) {
      cancelAnimationFrame(layoutAnimationFrameRef.current);
      layoutAnimationFrameRef.current = null;
    }
  };

  useEffect(() => {
    if (!containerRef.current || nodes.length === 0) return;

    stopLayoutAnimation();
    const graph = new Graph();

    const visibleNodes = nodes;
    const visibleNodeIds = new Set(visibleNodes.map(n => n.id));
    const visibleEdges = edges.filter((edge) =>
      edge.sourceId !== edge.targetId &&
      visibleNodeIds.has(edge.sourceId) &&
      visibleNodeIds.has(edge.targetId),
    );
    const denseGraph = visibleEdges.length > 2200 || visibleNodes.length > 700;
    const edgeColor = denseGraph ? GRAPH_EDGE_COLORS.dense : GRAPH_EDGE_COLORS.regular;
    const edgeSize = denseGraph ? 0.22 : 0.34;
    const labelDensity = visibleNodes.length > 900 ? 0.016 : visibleNodes.length > 450 ? 0.022 : 0.045;
    const prominentLabelIds = selectProminentGraphLabelIds(visibleNodes);
    const defaultEdgeType = denseGraph ? 'line' : 'curvedArrow';

    // Node radius shrinks with the visible node count so dense graphs do
    // not paint as a solid color block. The previous fixed clamp of 3..13
    // ignored density and produced overlapping discs as soon as the
    // dataset crossed ~5 000 nodes.
    //   * <500 nodes: full 3..13 px range (per-edge weight visible)
    //   * 500..5 000 nodes: 2..7 px range (still readable individually)
    //   * 5 000..15 000 nodes: 1.4..4 px range
    //   * >15 000 nodes: 1..2.6 px range (treat as density visualization)
    const densityClamp =
      visibleNodes.length > 15000
        ? { min: 1, max: 2.6, base: 1, factor: 0.18 }
        : visibleNodes.length > 5000
          ? { min: 1.4, max: 4, base: 1.4, factor: 0.28 }
          : visibleNodes.length > 500
            ? { min: 2, max: 7, base: 2, factor: 0.42 }
            : { min: 3, max: 13, base: 3, factor: 0.65 };

    for (const node of visibleNodes) {
      const color = GRAPH_NODE_COLORS[node.type] || GRAPH_NODE_COLORS.entity;
      const size = Math.max(
        densityClamp.min,
        Math.min(densityClamp.max, densityClamp.base + Math.sqrt(node.edgeCount) * densityClamp.factor),
      );
      const showLabel = prominentLabelIds.has(node.id);
      const canvasLabel = showLabel ? buildGraphCanvasLabel(node.label, visibleNodes.length) : '';
      graph.addNode(node.id, {
        label: canvasLabel,
        displayLabel: canvasLabel,
        originalLabel: node.label,
        focusLabel: buildGraphFocusLabel(node.label),
        x: 0,
        y: 0,
        size,
        color,
        nodeType: node.type,
        forceLabel: showLabel,
      });
    }

    const edgeSet = new Set<string>();
    for (const edge of visibleEdges) {
      if (!graph.hasNode(edge.sourceId) || !graph.hasNode(edge.targetId)) continue;
      const key = `${edge.sourceId}-${edge.targetId}`;
      if (edgeSet.has(key)) continue;
      edgeSet.add(key);
      try {
        graph.addEdge(edge.sourceId, edge.targetId, {
          label: edge.label || '',
          size: edgeSize,
          color: edgeColor,
          type: defaultEdgeType,
        });
      } catch { /* skip parallel */ }
    }

    applyGraphLayout(graph, layout);
    layoutRef.current = layout;

    graphRef.current = graph;
    if (sigmaRef.current) sigmaRef.current.kill();

    // Label-system tuning by graph density. The collision detection Sigma
    // runs for label placement is the dominant cost per frame on dense
    // graphs, and the thresholds below raise the bar on "is this node
    // large enough to deserve a label check at all" so the expensive
    // pass runs on far fewer nodes. `labelGridCellSize` tunes the spatial
    // hash used for label collisions — bigger cells = fewer cells =
    // cheaper lookup, at the cost of slightly looser deduplication.
    const ultraDenseGraph = visibleNodes.length > 5000;
    const labelRenderedSizeThreshold = visibleNodes.length > 15000
      ? 20
      : visibleNodes.length > 5000
        ? 14
        : visibleNodes.length > 900
          ? 10
          : 8;
    const labelGridCellSize = visibleNodes.length > 5000 ? 240 : 100;

    const sigma = new Sigma(graph, containerRef.current, {
      // Edges must stay visible during pan/zoom — hiding them mid-move
      // makes the graph feel broken and disconnected. The performance
      // tradeoff for very dense datasets is acceptable.
      hideEdgesOnMove: false,
      // On dense graphs, labels are skipped entirely during pan/zoom to
      // keep the frame budget under control; on small graphs the 140-node
      // threshold keeps the interactive feel of always-on labels.
      hideLabelsOnMove: ultraDenseGraph || visibleNodes.length > 140,
      renderLabels: true,
      renderEdgeLabels: false,
      labelFont: 'Inter, system-ui, sans-serif',
      labelSize: 12,
      labelWeight: '500',
      labelColor: { color: '#94a3b8' },
      defaultNodeColor: '#78716c',
      defaultEdgeColor: edgeColor,
      defaultEdgeType,
      edgeProgramClasses: {
        curvedArrow: EdgeCurvedArrowProgram,
      },
      labelDensity,
      labelGridCellSize,
      labelRenderedSizeThreshold,
      autoCenter: true,
      autoRescale: true,
      zIndex: true,
      minCameraRatio: 0.01,
      maxCameraRatio: 50,
      allowInvalidContainer: true,
    });

    // Faster zoom
    const camera = sigma.getCamera();
    const container = containerRef.current;
    const wheelHandler = (e: WheelEvent) => {
      e.preventDefault();
      const factor = e.deltaY > 0 ? 1.2 : 0.83;
      const newRatio = camera.ratio * factor;
      camera.animate({ ratio: Math.max(0.01, Math.min(50, newRatio)) }, { duration: 50 });
    };
    container.addEventListener('wheel', wheelHandler, { passive: false });

    // Node dragging
    let draggedNode: string | null = null;

    sigma.on('downNode', ({ node }) => {
      draggedNode = node;
      dragStateRef.current = { dragging: true, node };
      graph.setNodeAttribute(node, 'highlighted', true);
      sigma.getCamera().disable();
    });

    sigma.getMouseCaptor().on('mousemovebody', (e: any) => {
      if (!draggedNode) return;
      const pos = sigma.viewportToGraph(e);
      graph.setNodeAttribute(draggedNode, 'x', pos.x);
      graph.setNodeAttribute(draggedNode, 'y', pos.y);
      e.preventSigmaDefault();
      e.original.preventDefault();
      e.original.stopPropagation();
    });

    sigma.getMouseCaptor().on('mouseup', () => {
      if (draggedNode) {
        graph.removeNodeAttribute(draggedNode, 'highlighted');
        sigma.getCamera().enable();
        draggedNode = null;
        dragStateRef.current = { dragging: false, node: null };
      }
    });

    // Pointer cursor on node hover. Hover state is rAF-throttled via
    // `scheduleHoverUpdate` so cursor sweeps through dense graphs do not
    // queue dozens of React rerenders + sigma refreshes per second.
    //
    // We also drive a floating DOM tooltip with the node label + its
    // neighbor names. Tooltip is pure CSS/DOM — completely outside the
    // Sigma render path — so it works on dense graphs without paying the
    // ~120 ms `sigma.refresh()` cost per hover transition.
    sigma.on('enterNode', ({ node }) => {
      scheduleHoverUpdate(node);
      if (containerRef.current) containerRef.current.style.cursor = 'pointer';
      const neighborSet = neighborIndex.get(node);
      const neighborIds = neighborSet ? Array.from(neighborSet) : [];
      const neighborLabels = neighborIds
        .slice(0, 12)
        .map((id) => labelByNodeId.get(id) ?? id)
        .filter((label): label is string => !!label);
      const label =
        labelByNodeId.get(node) ??
        (graph.getNodeAttribute(node, 'originalLabel') as string | undefined) ??
        node;
      setTooltip({
        nodeId: node,
        label,
        neighborLabels,
        neighborCount: neighborIds.length,
      });
      // Anchor card to the node's viewport position — not the cursor.
      const updatePos = () => {
        const x = graph.getNodeAttribute(node, 'x') as number | undefined;
        const y = graph.getNodeAttribute(node, 'y') as number | undefined;
        if (x == null || y == null) return;
        const viewport = sigma.graphToViewport({ x, y });
        const containerRect = containerRef.current?.getBoundingClientRect();
        setTooltipPos({
          x: viewport.x + (containerRect?.left ?? 0),
          y: viewport.y + (containerRect?.top ?? 0),
        });
      };
      updatePos();
    });
    sigma.on('leaveNode', () => {
      scheduleHoverUpdate(null);
      if (containerRef.current) containerRef.current.style.cursor = 'default';
      setTooltip(null);
      setTooltipPos(null);
    });
    // Reposition the card on camera move so it stays glued to the node
    // when the user pans/zooms with the hover still active.
    sigma.getCamera().on('updated', () => {
      const current = tooltipRef.current;
      if (!current) return;
      const activeNodeId = current.dataset.nodeId;
      if (!activeNodeId || !graph.hasNode(activeNodeId)) return;
      const x = graph.getNodeAttribute(activeNodeId, 'x') as number | undefined;
      const y = graph.getNodeAttribute(activeNodeId, 'y') as number | undefined;
      if (x == null || y == null) return;
      const viewport = sigma.graphToViewport({ x, y });
      const containerRect = containerRef.current?.getBoundingClientRect();
      current.style.left = `${viewport.x + (containerRect?.left ?? 0) + 12}px`;
      current.style.top = `${viewport.y + (containerRect?.top ?? 0) + 12}px`;
    });

    sigma.on('clickNode', ({ node }) => {
      if (!dragStateRef.current.dragging) onSelect(node);
    });
    sigma.on('clickStage', () => {
      setHoveredId(null);
      if (!dragStateRef.current.dragging) onSelect(null);
    });

    sigmaRef.current = sigma;
    requestAnimationFrame(() => {
      void sigma.getCamera().animatedReset({ duration: 180 });
    });

    return () => {
      stopLayoutAnimation();
      if (hoverTimerRef.current != null) {
        clearTimeout(hoverTimerRef.current);
        hoverTimerRef.current = null;
      }
      pendingHoverRef.current = null;
      setHoveredId(null);
      setTooltip(null);
      container.removeEventListener('wheel', wheelHandler);
      sigma.kill();
      sigmaRef.current = null;
    };
  }, [nodes, edges, onSelect]);

  useEffect(() => {
    const sigma = sigmaRef.current;
    const graph = graphRef.current;
    if (!sigma || !graph || nodes.length === 0) return;
    if (layoutRef.current === layout) return;

    stopLayoutAnimation();
    layoutRef.current = layout;

    const targetGraph = cloneGraphStructure(graph);
    applyGraphLayout(targetGraph, layout);

    const transitionNodes = graph.nodes().map(node => ({
      node,
      fromX: (graph.getNodeAttribute(node, 'x') as number) ?? 0,
      fromY: (graph.getNodeAttribute(node, 'y') as number) ?? 0,
      toX: (targetGraph.getNodeAttribute(node, 'x') as number) ?? 0,
      toY: (targetGraph.getNodeAttribute(node, 'y') as number) ?? 0,
    }));

    const reduceMotion =
      typeof window !== 'undefined' &&
      typeof window.matchMedia === 'function' &&
      window.matchMedia('(prefers-reduced-motion: reduce)').matches;

    if (reduceMotion || transitionNodes.length === 0) {
      for (const transition of transitionNodes) {
        graph.setNodeAttribute(transition.node, 'x', transition.toX);
        graph.setNodeAttribute(transition.node, 'y', transition.toY);
      }
      sigma.refresh();
      void sigma.getCamera().animatedReset({ duration: 140 });
      return;
    }

    const animationToken = layoutAnimationTokenRef.current + 1;
    layoutAnimationTokenRef.current = animationToken;
    const startedAt = performance.now();

    const renderFrame = (now: number) => {
      if (layoutAnimationTokenRef.current !== animationToken) return;

      const progress = Math.min(1, (now - startedAt) / LAYOUT_ANIMATION_DURATION_MS);
      const eased = 1 - Math.pow(1 - progress, 3);

      for (const transition of transitionNodes) {
        graph.setNodeAttribute(
          transition.node,
          'x',
          transition.fromX + (transition.toX - transition.fromX) * eased,
        );
        graph.setNodeAttribute(
          transition.node,
          'y',
          transition.fromY + (transition.toY - transition.fromY) * eased,
        );
      }

      sigma.refresh();

      if (progress < 1) {
        layoutAnimationFrameRef.current = requestAnimationFrame(renderFrame);
      } else {
        layoutAnimationFrameRef.current = null;
        void sigma.getCamera().animatedReset({ duration: 180 });
      }
    };

    layoutAnimationFrameRef.current = requestAnimationFrame(renderFrame);

    return () => {
      stopLayoutAnimation();
    };
  }, [layout, nodes]);

  useEffect(() => {
    const sigma = sigmaRef.current;
    const graph = graphRef.current;
    if (!sigma || !graph) return;

    // Two distinct interaction modes:
    //
    // CLICK (selectedId set): full focus mode. Selected node + its edges
    // pop out, every other node fades to gray, every other edge fades.
    // Used when the user has explicitly picked a node to study.
    //
    // HOVER (hoveredId set, no selection): soft hint only. Highlight the
    // hovered node and its neighbors with a label + slight size bump, but
    // leave every other node and every edge untouched. Hovering over a
    // node should not visually rewrite the entire graph.
    //
    // When neither is set, clear both reducers so the graph renders at
    // its base style.
    if (selectedId && graph.hasNode(selectedId)) {
      const connectedEdges = new Set<string>();
      // Use the precomputed neighbor index instead of `graph.neighbors`
      // so the lookup is O(1) instead of walking the adjacency list.
      const neighbors = neighborIndex.get(selectedId) ?? new Set<string>();
      graph.forEachEdge((edge) => {
        if (graph.source(edge) === selectedId || graph.target(edge) === selectedId) {
          connectedEdges.add(edge);
        }
      });

      sigma.setSetting('nodeReducer', (node: string, data: any) => {
        const isActive = node === selectedId;
        const isNeighbor = neighbors.has(node);
        if (isActive) {
          return {
            ...data,
            zIndex: 4,
            size: Math.max((data.size ?? 0) as number, 9),
            label: data.focusLabel ?? data.displayLabel ?? data.label,
            forceLabel: true,
            highlighted: true,
          };
        }
        if (isNeighbor) {
          return {
            ...data,
            zIndex: 3,
            size: Math.max((data.size ?? 0) as number, 7),
            label: data.focusLabel ?? data.displayLabel ?? data.label,
            forceLabel: true,
          };
        }
        return {
          ...data,
          color: '#ffffff',
          zIndex: 0,
          size: Math.max((data.size ?? 0) as number, 2),
          label: '',
        };
      });

      sigma.setSetting('edgeReducer', (edge: string, data: any) => {
        if (connectedEdges.has(edge)) {
          return {
            ...data,
            color: GRAPH_EDGE_COLORS.highlight,
            size: Math.max((data.size ?? 0) as number, 0.8),
            zIndex: 4,
          };
        }
        return {
          ...data,
          color: '#ffffff',
          size: 0.05,
          zIndex: 0,
        };
      });
    } else if (hoveredId && graph.hasNode(hoveredId)) {
      // The dwell-time gate (`HOVER_DWELL_MS`) ensures this branch only
      // runs when the user actually pauses on a node, not on every
      // mousemove. So we can afford a real `nodeReducer` here that bumps
      // both the hovered node and its neighbors with labels — the
      // ~120 ms refresh happens once per intentional hover, not 60 times
      // per second during a sweep.
      const neighbors = neighborIndex.get(hoveredId) ?? new Set<string>();
      sigma.setSetting('nodeReducer', (node: string, data: any) => {
        if (node === hoveredId) {
          return {
            ...data,
            zIndex: 4,
            size: Math.max((data.size ?? 0) as number, 11),
            label: data.focusLabel ?? data.displayLabel ?? data.label,
            forceLabel: true,
            highlighted: true,
          };
        }
        if (neighbors.has(node)) {
          return {
            ...data,
            zIndex: 3,
            size: Math.max((data.size ?? 0) as number, 8),
            label: data.focusLabel ?? data.displayLabel ?? data.label,
            forceLabel: true,
          };
        }
        return data;
      });
      // Edges stay untouched on hover.
      sigma.setSetting('edgeReducer', null);
    } else {
      sigma.setSetting('nodeReducer', null);
      sigma.setSetting('edgeReducer', null);
    }

    sigma.refresh();
  }, [hoveredId, selectedId, nodes]);

  return (
    <>
      <div ref={containerRef} className="w-full h-full" style={{ minHeight: '400px' }} />
      {tooltip && tooltipPos && (
        <div
          ref={tooltipRef}
          data-node-id={tooltip.nodeId}
          className="fixed pointer-events-none z-50 max-w-xs rounded-md border border-border bg-popover/95 px-3 py-2 text-xs text-popover-foreground shadow-lg backdrop-blur-sm"
          style={{ left: tooltipPos.x + 12, top: tooltipPos.y + 12 }}
        >
          <div className="font-semibold text-sm leading-tight mb-1 truncate">{tooltip.label}</div>
          <div className="text-muted-foreground text-[11px] mb-1">
            {tooltip.neighborCount} {tooltip.neighborCount === 1 ? 'связь' : 'связей'}
          </div>
          {tooltip.neighborLabels.length > 0 && (
            <ul className="space-y-0.5 list-disc list-inside text-[11px] text-muted-foreground">
              {tooltip.neighborLabels.map((label, i) => (
                <li key={i} className="truncate">{label}</li>
              ))}
              {tooltip.neighborCount > tooltip.neighborLabels.length && (
                <li className="text-muted-foreground/70">…ещё {tooltip.neighborCount - tooltip.neighborLabels.length}</li>
              )}
            </ul>
          )}
        </div>
      )}
    </>
  );
}
