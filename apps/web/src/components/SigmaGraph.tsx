import { useEffect, useRef, useState } from 'react';
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
  const sigmaRef = useRef<Sigma | null>(null);
  const graphRef = useRef<Graph | null>(null);
  const dragStateRef = useRef<{ dragging: boolean; node: string | null }>({ dragging: false, node: null });
  const [hoveredId, setHoveredId] = useState<string | null>(null);
  const layoutRef = useRef(layout);
  const layoutAnimationFrameRef = useRef<number | null>(null);
  const layoutAnimationTokenRef = useRef(0);

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

    for (const node of visibleNodes) {
      const color = GRAPH_NODE_COLORS[node.type] || GRAPH_NODE_COLORS.entity;
      const size = Math.max(3, Math.min(13, 3 + Math.sqrt(node.edgeCount) * 0.65));
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

    const sigma = new Sigma(graph, containerRef.current, {
      hideEdgesOnMove: denseGraph,
      hideLabelsOnMove: visibleNodes.length > 140,
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
      labelGridCellSize: 100,
      labelRenderedSizeThreshold: visibleNodes.length > 900 ? 10 : 8,
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

    // Pointer cursor on node hover
    sigma.on('enterNode', ({ node }) => {
      setHoveredId(node);
      if (containerRef.current) containerRef.current.style.cursor = 'pointer';
    });
    sigma.on('leaveNode', ({ node }) => {
      setHoveredId((current) => (current === node ? null : current));
      if (containerRef.current) containerRef.current.style.cursor = 'default';
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
      setHoveredId(null);
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

    const activeId = selectedId ?? hoveredId;

    if (activeId && graph.hasNode(activeId)) {
      const connectedEdges = new Set<string>();
      const neighbors = new Set(graph.neighbors(activeId));
      graph.forEachEdge((edge) => {
        if (graph.source(edge) === activeId || graph.target(edge) === activeId) {
          connectedEdges.add(edge);
        }
      });

      sigma.setSetting('nodeReducer', (node: string, data: any) => {
        const isActive = node === activeId;
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
          color: '#d4d4d8',
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
            size: Math.max((data.size ?? 0) as number, 0.6),
          };
        }
        return {
          ...data,
          color: GRAPH_EDGE_COLORS.muted,
          size: Math.max((data.size ?? 0) as number, 0.2),
        };
      });
    } else {
      sigma.setSetting('nodeReducer', null);
      sigma.setSetting('edgeReducer', null);
    }

    sigma.refresh();
  }, [hoveredId, selectedId, nodes]);

  return (
    <div ref={containerRef} className="w-full h-full" style={{ minHeight: '400px' }} />
  );
}
