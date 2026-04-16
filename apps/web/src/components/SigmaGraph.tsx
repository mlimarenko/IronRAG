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
import { computeGraphLayoutOffThread } from '@/workers/graphLayoutClient';

interface EdgeData {
  id: string;
  sourceId: string;
  targetId: string;
  label: string;
  weight: number;
}

interface SigmaGraphProps {
  /** Full topology, not a filtered projection. Re-building the Graphology
   *  instance on every keystroke is a catastrophic cost on 100k-node graphs
   *  (seconds of layout + re-init per key), so filters are applied via
   *  Sigma's reducer pipeline instead of by rebuilding the graph. */
  nodes: GraphNode[];
  edges: EdgeData[];
  selectedId: string | null;
  onSelect: (id: string | null) => void;
  layout: GraphLayoutType;
  /** Canonical "hide this node" set. Empty means everything visible.
   *  Owned by the parent so search / legend toggles can drive the filter
   *  without touching the Graphology instance. */
  hiddenIds?: Set<string>;
}

type SigmaPointerCaptorEvent = {
  x: number;
  y: number;
  preventSigmaDefault: () => void;
  original: MouseEvent;
};

type SigmaReducerData = {
  size?: number;
  label?: string;
  displayLabel?: string;
  focusLabel?: string;
  highlighted?: boolean;
  [key: string]: unknown;
};

const LAYOUT_ANIMATION_DURATION_MS = 280;
/// Stable empty-set sentinel for hidden-edge lookups. Using one shared
/// reference avoids allocating a throwaway `new Set()` inside the hot
/// reducer effect on every run.
const EMPTY_EDGE_SET: ReadonlySet<string> = new Set();
/// Matching empty-set sentinel for the prominent-label lookup. Skipping
/// the O(N log N) sort inside `selectProminentGraphLabelIds` at
/// ultra-dense node counts means we short-circuit to this shared set
/// instead of allocating an empty one per rebuild.
const EMPTY_LABEL_SET: ReadonlySet<string> = new Set();
/// Above this node count, layout transitions are applied instantly
/// (no per-frame interpolation). At 5000+ nodes the animation burns
/// 1.5M setNodeAttribute calls per second and provides no visual
/// value — the human eye cannot track thousands of dots drifting at
/// once. Matches the density tier used for label throttling above.
const INSTANT_LAYOUT_NODE_THRESHOLD = 5000;
/// Above this node count, labels are disabled entirely. Sigma's label
/// collision detection is the dominant cost per frame even with
/// `hideLabelsOnMove` and `labelRenderedSizeThreshold` tuned up; at
/// 15k+ nodes the labels are visually useless anyway (unreadable at
/// that density) and turning them off shaves 30-50% off the per-frame
/// budget on the Artix fixture.
const LABELS_DISABLED_NODE_THRESHOLD = 15000;
/// Above this node count, the initial layout is computed in a Web
/// Worker so it never blocks the main thread. Below it, the sync
/// codepath is cheaper: serializing the node/edge arrays, spinning up
/// a postMessage round-trip, and deserializing the float positions is
/// ~20 ms of overhead that is not recovered on tiny graphs. 3000 is
/// roughly where `applyGraphLayout` starts to exceed a 16 ms frame
/// budget, so the crossover lines up naturally.
const GRAPH_WORKER_NODE_THRESHOLD = 3000;

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

export default function SigmaGraph({ nodes, edges, selectedId, onSelect, layout, hiddenIds }: SigmaGraphProps) {
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
  }, [edges]);

  // Cheap `nodeId -> label` lookup so the DOM tooltip can resolve names
  // without touching the Sigma graph instance. Built once per `nodes`
  // change, O(N) memory.
  const labelByNodeId = useMemo(() => {
    const map = new Map<string, string>();
    for (const n of nodes) map.set(n.id, n.label);
    return map;
  }, [nodes]);

  // Hidden-edge precompute. Owned by a ref that is rebuilt whenever the
  // graph rebuilds OR when `hiddenIds` changes. The reducer effect below
  // fires on every `hoveredId` change (once per intentional hover
  // commit); walking `graph.forEachEdge()` inside that effect would
  // repeatedly pay an O(M) scan on dense graphs where the user is
  // actively pointing. Precomputing once lets the reducer branches do an
  // O(1) `Set.has(edge)` check per edge per frame instead.
  const hiddenEdgeIdsRef = useRef<Set<string> | null>(null);

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

    // Cancellation gate. The build path can be async (Web Worker
    // layout), so if the effect is re-run (topology change, layout
    // change, unmount) before the worker resolves, we must abort the
    // half-built state instead of creating a zombie Sigma instance.
    const buildToken = { cancelled: false };
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
    // At ultra-dense node counts labels are disabled entirely below, so
    // `selectProminentGraphLabelIds` — which does an O(N log N) full
    // sort on the node array — is pointless work. Skipping it shaves
    // ~15 ms off the initial build on the Artix fixture.
    const prominentLabelIds =
      visibleNodes.length > LABELS_DISABLED_NODE_THRESHOLD
        ? EMPTY_LABEL_SET
        : selectProminentGraphLabelIds(visibleNodes);
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

    // Compute layout either synchronously or off-main-thread. The
    // worker path sends a minimal {id, nodeType, size, label} payload
    // and receives an interleaved Float32Array of positions via a
    // transferable buffer — no structured-clone copy of the full
    // topology, no main-thread Graphology build twice. For graphs
    // below `GRAPH_WORKER_NODE_THRESHOLD` the sync codepath wins
    // because the postMessage round-trip is pure overhead.
    const useWorker = visibleNodes.length >= GRAPH_WORKER_NODE_THRESHOLD;
    const layoutComputation: Promise<void> = useWorker
      ? (async () => {
          try {
            const workerNodes = visibleNodes.map((node) => ({
              id: node.id,
              nodeType: node.type,
              size:
                (graph.getNodeAttribute(node.id, 'size') as number | undefined) ?? 1,
              label: node.label,
            }));
            const workerEdges = visibleEdges.map((edge) => ({
              sourceId: edge.sourceId,
              targetId: edge.targetId,
            }));
            const result = await computeGraphLayoutOffThread({
              nodes: workerNodes,
              edges: workerEdges,
              layout,
            });
            if (buildToken.cancelled) return;
            for (let i = 0; i < result.ids.length; i += 1) {
              const id = result.ids[i];
              if (!graph.hasNode(id)) continue;
              graph.setNodeAttribute(id, 'x', result.positions[i * 2]);
              graph.setNodeAttribute(id, 'y', result.positions[i * 2 + 1]);
            }
          } catch (error) {
            // Worker failed (bundler misconfig, OOM, whatever). Fall
            // back to the synchronous layout path so the graph still
            // renders, even if it briefly freezes the main thread.
            if (buildToken.cancelled) return;
            // eslint-disable-next-line no-console
            console.warn('[graph] worker layout failed, falling back to main thread', error);
            applyGraphLayout(graph, layout);
          }
        })()
      : Promise.resolve().then(() => {
          applyGraphLayout(graph, layout);
        });

    let wheelHandler: ((e: WheelEvent) => void) | null = null;
    let sigmaInstance: Sigma | null = null;
    const containerAtMount = containerRef.current;

    void layoutComputation.then(() => {
      if (buildToken.cancelled) return;
      if (!containerRef.current) return;
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
    const labelsDisabled = visibleNodes.length > LABELS_DISABLED_NODE_THRESHOLD;
    const labelRenderedSizeThreshold = labelsDisabled
      ? 9999
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
      // Disabling `renderLabels` at ultra-dense node counts cuts the
      // Sigma per-frame cost by 30-50% (Sigma's label collision pass
      // is the dominant hot path at 15k+ nodes) with no visual loss
      // because individual labels are unreadable at that density.
      renderLabels: !labelsDisabled,
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

    sigmaInstance = sigma;

    // Faster zoom
    const camera = sigma.getCamera();
    const container = containerRef.current;
    wheelHandler = (e: WheelEvent) => {
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

    sigma.getMouseCaptor().on('mousemovebody', (e: SigmaPointerCaptorEvent) => {
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
    });

    return () => {
      // Abort any in-flight worker layout before the cleanup runs so
      // the `.then` body short-circuits before it ever touches Sigma.
      buildToken.cancelled = true;
      stopLayoutAnimation();
      if (hoverTimerRef.current != null) {
        clearTimeout(hoverTimerRef.current);
        hoverTimerRef.current = null;
      }
      pendingHoverRef.current = null;
      setHoveredId(null);
      setTooltip(null);
      if (containerAtMount && wheelHandler) {
        containerAtMount.removeEventListener('wheel', wheelHandler);
      }
      if (sigmaInstance) {
        sigmaInstance.kill();
      }
      sigmaRef.current = null;
    };
  }, [nodes, edges, labelByNodeId, layout, neighborIndex, onSelect]);

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

    // Skip per-frame interpolation on ultra-dense graphs — the human
    // eye cannot track 5k+ nodes drifting simultaneously, and
    // setNodeAttribute calls at 60 fps × N nodes exceed the frame
    // budget anyway.
    const skipAnimation =
      reduceMotion ||
      transitionNodes.length === 0 ||
      transitionNodes.length >= INSTANT_LAYOUT_NODE_THRESHOLD;

    if (skipAnimation) {
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

  // Recompute hidden edge ids whenever `hiddenIds` (or the underlying
  // topology) changes — O(M) once per change instead of O(M) once per
  // hover. The ref is read by the reducer effect below without
  // triggering its own re-run, so hover transitions do not pay the
  // scan cost.
  useEffect(() => {
    const graph = graphRef.current;
    if (!graph) {
      hiddenEdgeIdsRef.current = null;
      return;
    }
    if (!hiddenIds || hiddenIds.size === 0) {
      hiddenEdgeIdsRef.current = null;
      return;
    }
    const hidden = new Set<string>();
    graph.forEachEdge((edge, _attrs, source, target) => {
      if (hiddenIds.has(source) || hiddenIds.has(target)) {
        hidden.add(edge);
      }
    });
    hiddenEdgeIdsRef.current = hidden;
  }, [hiddenIds, nodes, edges]);

  useEffect(() => {
    const sigma = sigmaRef.current;
    const graph = graphRef.current;
    if (!sigma || !graph) return;

    // Filters are applied through Sigma's reducer pipeline — never by
    // rebuilding the Graphology instance. On a 100k-node / 100k-edge graph
    // a teardown + layout + re-init burns multiple seconds per keystroke;
    // the reducer path runs in a few milliseconds because Graphology state
    // is untouched.
    //
    // Hidden-edge set is owned by `hiddenEdgeIdsRef` (built by the
    // dedicated effect above). Reading a ref here keeps the reducer
    // effect off the hidden-edge dependency graph — hover transitions
    // would otherwise rerun the O(M) scan even when `hiddenIds` is
    // unchanged.
    const hiddenNodeSet = hiddenIds && hiddenIds.size > 0 ? hiddenIds : null;
    const hiddenEdgeIds = hiddenEdgeIdsRef.current ?? EMPTY_EDGE_SET;

    // Three distinct interaction modes (all composed with the filter):
    //
    // CLICK (selectedId set): full focus mode. Selected node + its edges
    // pop out, every other node fades to gray, every other edge fades.
    //
    // HOVER (hoveredId set, no selection): soft hint only. Highlight the
    // hovered node and its neighbors with a label + slight size bump.
    //
    // IDLE: either a pure filter pass (when hiddenIds is non-empty) or
    // null reducers so the graph renders at its base style.
    //
    // The hidden check must run FIRST in every branch so filters always
    // win over selection/hover highlighting.
    if (selectedId && graph.hasNode(selectedId)) {
      // `graph.edges(node)` returns only the edges incident to
      // `selectedId` — O(degree) instead of O(M). The previous code
      // walked ALL 82k edges on the Artix fixture every time the user
      // clicked a node, which was visibly janky.
      const connectedEdges = new Set<string>(graph.edges(selectedId));
      const neighbors = neighborIndex.get(selectedId) ?? new Set<string>();

      sigma.setSetting('nodeReducer', (node: string, data: SigmaReducerData) => {
        if (hiddenNodeSet && hiddenNodeSet.has(node)) {
          return { ...data, hidden: true, label: '' };
        }
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

      sigma.setSetting('edgeReducer', (edge: string, data: SigmaReducerData) => {
        if (hiddenEdgeIds.has(edge)) {
          return { ...data, hidden: true };
        }
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
      sigma.setSetting('nodeReducer', (node: string, data: SigmaReducerData) => {
        if (hiddenNodeSet && hiddenNodeSet.has(node)) {
          return { ...data, hidden: true, label: '' };
        }
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
      if (hiddenNodeSet) {
        sigma.setSetting('edgeReducer', (edge: string, data: SigmaReducerData) => {
          if (hiddenEdgeIds.has(edge)) return { ...data, hidden: true };
          return data;
        });
      } else {
        sigma.setSetting('edgeReducer', null);
      }
    } else if (hiddenNodeSet) {
      // Pure filter mode: no selection, no hover, but filters are active.
      // Hide nodes/edges without touching anything else.
      sigma.setSetting('nodeReducer', (node: string, data: SigmaReducerData) => {
        if (hiddenNodeSet.has(node)) return { ...data, hidden: true, label: '' };
        return data;
      });
      sigma.setSetting('edgeReducer', (edge: string, data: SigmaReducerData) => {
        if (hiddenEdgeIds.has(edge)) return { ...data, hidden: true };
        return data;
      });
    } else {
      sigma.setSetting('nodeReducer', null);
      sigma.setSetting('edgeReducer', null);
    }

    sigma.refresh();
  }, [hoveredId, neighborIndex, selectedId, hiddenIds]);

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
