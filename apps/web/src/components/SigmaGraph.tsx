import { useEffect, useRef } from 'react';
import Graph from 'graphology';
import Sigma from 'sigma';
import { EdgeCurvedArrowProgram } from '@sigma/edge-curve';
import type { GraphNode } from '@/types';
import { GRAPH_NODE_COLORS, type GraphLayoutType } from '@/components/graph/config';
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
  hiddenTypes: Set<string>;
}

const LAYOUT_ANIMATION_DURATION_MS = 280;
const GRAPH_EDGE_COLORS = {
  dense: 'rgba(71, 85, 105, 0.34)',
  regular: 'rgba(71, 85, 105, 0.5)',
  muted: 'rgba(71, 85, 105, 0.24)',
} as const;

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

export default function SigmaGraph({ nodes, edges, selectedId, onSelect, layout, hiddenTypes }: SigmaGraphProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const sigmaRef = useRef<Sigma | null>(null);
  const graphRef = useRef<Graph | null>(null);
  const dragStateRef = useRef<{ dragging: boolean; node: string | null }>({ dragging: false, node: null });
  const selectedIdRef = useRef<string | null>(selectedId);
  const selectedEdgesRef = useRef<EdgeData[]>([]);
  const selectionOverlayRenderRef = useRef<(() => void) | null>(null);
  const layoutRef = useRef(layout);
  const layoutAnimationFrameRef = useRef<number | null>(null);
  const layoutAnimationTokenRef = useRef(0);

  selectedIdRef.current = selectedId;

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

    const visibleNodes = hiddenTypes.size > 0
      ? nodes.filter(n => !hiddenTypes.has(n.type))
      : nodes;
    const visibleNodeIds = new Set(visibleNodes.map(n => n.id));
    const visibleEdges = edges.filter((edge) =>
      edge.sourceId !== edge.targetId &&
      visibleNodeIds.has(edge.sourceId) &&
      visibleNodeIds.has(edge.targetId),
    );
    const denseGraph = visibleEdges.length > 2200 || visibleNodes.length > 700;
    const edgeColor = denseGraph ? GRAPH_EDGE_COLORS.dense : GRAPH_EDGE_COLORS.regular;
    const edgeSize = denseGraph ? 0.18 : 0.3;
    const labelDensity = visibleNodes.length > 900 ? 0.03 : visibleNodes.length > 450 ? 0.045 : 0.07;

    for (const node of visibleNodes) {
      const color = GRAPH_NODE_COLORS[node.type] || GRAPH_NODE_COLORS.entity;
      const size = Math.max(3, Math.min(13, 3 + Math.sqrt(node.edgeCount) * 0.65));
      graph.addNode(node.id, {
        label: node.label,
        x: 0,
        y: 0,
        size,
        color,
        nodeType: node.type,
        forceLabel: node.type === 'document' || node.edgeCount >= 24,
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
        });
      } catch { /* skip parallel */ }
    }

    applyGraphLayout(graph, layout);
    layoutRef.current = layout;

    graphRef.current = graph;
    if (sigmaRef.current) sigmaRef.current.kill();

    const sigma = new Sigma(graph, containerRef.current, {
      hideEdgesOnMove: denseGraph,
      hideLabelsOnMove: visibleNodes.length > 350,
      renderLabels: true,
      renderEdgeLabels: false,
      labelFont: 'Inter, system-ui, sans-serif',
      labelSize: 12,
      labelWeight: '500',
      labelColor: { color: '#94a3b8' },
      defaultNodeColor: '#78716c',
      defaultEdgeColor: edgeColor,
      defaultEdgeType: 'curvedArrow',
      edgeProgramClasses: {
        curvedArrow: EdgeCurvedArrowProgram,
      },
      labelDensity,
      labelGridCellSize: 100,
      labelRenderedSizeThreshold: visibleNodes.length > 900 ? 9 : 7,
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

    // Highlight overlay: official Sigma layer API — Canvas2D ABOVE nodes
    // Layer order: edges → nodes → **highlightEdges** → labels → hovers
    const hlCanvas = sigma.createCanvas('highlightEdges', {
      afterLayer: 'nodes',
      style: { pointerEvents: 'none' },
    });
    const hlCtx = hlCanvas.getContext('2d');

    const renderHighlight = () => {
      if (!hlCtx) return;
      const { width, height } = sigma.getDimensions();
      const pr = window.devicePixelRatio || 1;
      const cw = Math.round(width * pr);
      const ch = Math.round(height * pr);
      if (hlCanvas.width !== cw || hlCanvas.height !== ch) {
        hlCanvas.width = cw;
        hlCanvas.height = ch;
        hlCanvas.style.width = `${width}px`;
        hlCanvas.style.height = `${height}px`;
      }
      hlCtx.setTransform(pr, 0, 0, pr, 0, 0);
      hlCtx.clearRect(0, 0, width, height);

      const sid = selectedIdRef.current;
      const hEdges = selectedEdgesRef.current;
      if (!sid || hEdges.length === 0 || !graph.hasNode(sid)) return;

      hlCtx.strokeStyle = 'rgba(59, 130, 246, 0.45)';
      hlCtx.lineWidth = 1.5;
      hlCtx.lineCap = 'round';

      for (const edge of hEdges) {
        if (!graph.hasNode(edge.sourceId) || !graph.hasNode(edge.targetId)) continue;
        const s = sigma.graphToViewport({
          x: graph.getNodeAttribute(edge.sourceId, 'x'),
          y: graph.getNodeAttribute(edge.sourceId, 'y'),
        });
        const t = sigma.graphToViewport({
          x: graph.getNodeAttribute(edge.targetId, 'x'),
          y: graph.getNodeAttribute(edge.targetId, 'y'),
        });

        // Curved arc — perpendicular offset proportional to distance
        const dx = t.x - s.x;
        const dy = t.y - s.y;
        const dist = Math.hypot(dx, dy) || 1;
        const nx = -dy / dist;
        const ny = dx / dist;
        const curvature = Math.min(40, dist * 0.15);
        const cx = (s.x + t.x) / 2 + nx * curvature;
        const cy = (s.y + t.y) / 2 + ny * curvature;

        hlCtx.beginPath();
        hlCtx.moveTo(s.x, s.y);
        hlCtx.quadraticCurveTo(cx, cy, t.x, t.y);
        hlCtx.stroke();
      }
    };

    sigma.on('afterRender', renderHighlight);
    selectionOverlayRenderRef.current = renderHighlight;

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

    // Pointer cursor on node hover
    sigma.on('enterNode', () => {
      if (containerRef.current) containerRef.current.style.cursor = 'pointer';
    });
    sigma.on('leaveNode', () => {
      if (containerRef.current) containerRef.current.style.cursor = 'default';
    });

    sigma.on('clickNode', ({ node }) => {
      if (!dragStateRef.current.dragging) onSelect(node);
    });
    sigma.on('clickStage', () => {
      if (!dragStateRef.current.dragging) onSelect(null);
    });

    sigmaRef.current = sigma;
    requestAnimationFrame(() => {
      void sigma.getCamera().animatedReset({ duration: 180 });
    });

    return () => {
      stopLayoutAnimation();
      container.removeEventListener('wheel', wheelHandler);
      sigma.off('afterRender', renderHighlight);
      selectionOverlayRenderRef.current = null;
      sigma.kill();
      sigmaRef.current = null;
    };
  }, [nodes, edges, onSelect, hiddenTypes]);

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

  // Selection highlighting via reducers + dedicated top overlay for active edges.
  useEffect(() => {
    const sigma = sigmaRef.current;
    const graph = graphRef.current;
    if (!sigma || !graph) return;

    if (selectedId && graph.hasNode(selectedId)) {
      const neighbors = new Set(graph.neighbors(selectedId));
      const renderedSelectedEdges: EdgeData[] = [];
      const seenEdges = new Set<string>();

      for (const edge of edges) {
        const isConnected = edge.sourceId === selectedId || edge.targetId === selectedId;
        if (!isConnected) continue;
        if (!graph.hasNode(edge.sourceId) || !graph.hasNode(edge.targetId)) continue;

        const edgeKey = `${edge.sourceId}:${edge.targetId}:${edge.label}`;
        if (seenEdges.has(edgeKey)) continue;
        seenEdges.add(edgeKey);
        renderedSelectedEdges.push(edge);
      }

      selectedEdgesRef.current = renderedSelectedEdges;

      sigma.setSetting('nodeReducer', (node: string, data: any) => {
        const isSelected = node === selectedId;
        const isNeighbor = neighbors.has(node);
        if (isSelected) {
          return { ...data, zIndex: 2, label: data.label, highlighted: true };
        }
        if (isNeighbor) {
          return { ...data, zIndex: 1, label: data.label };
        }
        return { ...data, color: '#d4d4d8', zIndex: 0, label: '' };
      });

      sigma.setSetting('edgeReducer', (edge: string, data: any) => {
        const source = graph.source(edge);
        const target = graph.target(edge);
        const isConnected = source === selectedId || target === selectedId;
        if (isConnected) {
          // Hide connected edges from normal WebGL layer — they'll be drawn
          // on the highlight overlay ABOVE nodes instead
          return { ...data, hidden: true };
        }
        return { ...data, color: GRAPH_EDGE_COLORS.muted, size: 0.15 };
      });
    } else {
      selectedEdgesRef.current = [];
      sigma.setSetting('nodeReducer', null);
      sigma.setSetting('edgeReducer', null);
    }

    sigma.refresh();
    selectionOverlayRenderRef.current?.();
  }, [selectedId, edges, nodes, hiddenTypes]);

  return (
    <div ref={containerRef} className="w-full h-full" style={{ minHeight: '400px' }} />
  );
}
