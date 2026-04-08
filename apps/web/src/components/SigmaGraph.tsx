import { useEffect, useRef } from 'react';
import Graph from 'graphology';
import Sigma from 'sigma';
import { EdgeCurvedArrowProgram } from '@sigma/edge-curve';
import circular from 'graphology-layout/circular';
import type { GraphNode } from '@/types';

const NODE_COLORS: Record<string, string> = {
  document: '#3b82f6',
  person: '#ec4899',
  organization: '#64748b',
  location: '#84cc16',
  event: '#f43f5e',
  artifact: '#06b6d4',
  natural: '#22c55e',
  process: '#a855f7',
  concept: '#f59e0b',
  attribute: '#0ea5e9',
  entity: '#78716c',
};

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
  layout: string;
  hiddenTypes: Set<string>;
}

// --- Layout helpers ---

function groupByType(graph: Graph): Map<string, string[]> {
  const groups = new Map<string, string[]>();
  graph.forEachNode((node, attrs) => {
    const t = (attrs.nodeType as string) || 'entity';
    if (!groups.has(t)) groups.set(t, []);
    groups.get(t)!.push(node);
  });
  return groups;
}

// --- Layout algorithms ---

/** Simple circle — one ring for all nodes */
function layoutCircle(graph: Graph): void {
  circular.assign(graph);
  // Scale radius so nodes aren't too cramped at large N
  const n = graph.order;
  if (n > 100) {
    const s = Math.sqrt(n) * 0.15;
    graph.forEachNode((node, attrs) => {
      graph.setNodeAttribute(node, 'x', (attrs.x as number) * s);
      graph.setNodeAttribute(node, 'y', (attrs.y as number) * s);
    });
  }
}

/** Cloud — circular base + edge-based jitter (fast, no O(n²) sim) */
function layoutCloud(graph: Graph): void {
  const n = graph.order;
  if (n === 0) return;

  // Start from circular, then add randomness proportional to node count
  circular.assign(graph);
  const jitter = Math.sqrt(n) * 1.5;
  graph.forEachNode((node, attrs) => {
    graph.setNodeAttribute(node, 'x', (attrs.x as number) * jitter + (Math.random() - 0.5) * jitter * 0.8);
    graph.setNodeAttribute(node, 'y', (attrs.y as number) * jitter + (Math.random() - 0.5) * jitter * 0.8);
  });
}

/** Concentric rings — each type on its own ring, well-separated */
function layoutRings(graph: Graph): void {
  const n = graph.order;
  if (n === 0) return;

  const groups = groupByType(graph);
  const types = Array.from(groups.keys()).sort((a, b) => (groups.get(b)!.length - groups.get(a)!.length));
  const typeCount = types.length;

  // Use an explicit inter-ring gap so distant zoom levels still read as distinct bands.
  const baseRingGap = Math.max(12, Math.sqrt(n) * 0.55);
  const minArcGap = Math.max(2.4, baseRingGap * 0.48);
  let currentRadius = Math.max(baseRingGap * 2, 6);

  for (let ring = 0; ring < types.length; ring++) {
    const nodesInRing = groups.get(types[ring])!;
    const radiusForCount = (nodesInRing.length * minArcGap) / (2 * Math.PI);
    const ringGap =
      ring === 0
        ? baseRingGap * 2
        : baseRingGap * (1 + ring / Math.max(1, typeCount - 1)) + Math.sqrt(currentRadius) * 0.7;

    currentRadius = ring === 0
      ? Math.max(baseRingGap * 2.5, radiusForCount)
      : Math.max(radiusForCount, currentRadius + ringGap);

    const angularOffset = ring * (Math.PI / Math.max(6, nodesInRing.length));

    for (let i = 0; i < nodesInRing.length; i++) {
      const angle = (2 * Math.PI * i) / nodesInRing.length + angularOffset;
      graph.setNodeAttribute(nodesInRing[i], 'x', Math.cos(angle) * currentRadius);
      graph.setNodeAttribute(nodesInRing[i], 'y', Math.sin(angle) * currentRadius);
    }
  }
}

/** Horizontal lanes — each type is a row, nodes spread across X */
function layoutLanes(graph: Graph): void {
  const n = graph.order;
  if (n === 0) return;

  const groups = groupByType(graph);
  const types = Array.from(groups.keys()).sort();

  // Lane height: enough vertical space between rows so they're clearly separate
  const maxGroupSize = Math.max(...Array.from(groups.values()).map(g => g.length));
  const laneWidth = maxGroupSize * 1.2;
  const laneHeight = Math.max(laneWidth * 0.12, 15);

  for (let lane = 0; lane < types.length; lane++) {
    const nodesInLane = groups.get(types[lane])!;
    const y = lane * laneHeight;
    const gap = laneWidth / Math.max(1, nodesInLane.length);
    for (let i = 0; i < nodesInLane.length; i++) {
      graph.setNodeAttribute(nodesInLane[i], 'x', (i - nodesInLane.length / 2) * gap);
      graph.setNodeAttribute(nodesInLane[i], 'y', y);
    }
  }
}

/** Clusters — type groups placed as bubbles around a circle, well separated */
function layoutClusters(graph: Graph): void {
  const n = graph.order;
  if (n === 0) return;

  const groups = groupByType(graph);
  const types = Array.from(groups.keys()).sort();

  // Place centroids far apart — orbit radius proportional to sqrt(n)
  const orbitRadius = Math.sqrt(n) * 8;

  for (let ci = 0; ci < types.length; ci++) {
    const nodesInCluster = groups.get(types[ci])!;
    const centroidAngle = (2 * Math.PI * ci) / types.length;
    const cx = Math.cos(centroidAngle) * orbitRadius;
    const cy = Math.sin(centroidAngle) * orbitRadius;

    // Spread nodes in a disc around centroid (Sunflower / Vogel pattern for even fill)
    const clusterRadius = Math.sqrt(nodesInCluster.length) * 2.5;
    const goldenAngle = Math.PI * (3 - Math.sqrt(5));
    for (let i = 0; i < nodesInCluster.length; i++) {
      const r = clusterRadius * Math.sqrt((i + 0.5) / nodesInCluster.length);
      const theta = i * goldenAngle;
      graph.setNodeAttribute(nodesInCluster[i], 'x', cx + Math.cos(theta) * r);
      graph.setNodeAttribute(nodesInCluster[i], 'y', cy + Math.sin(theta) * r);
    }
  }
}

/** Islands — connected components laid out separately in a grid */
function layoutIslands(graph: Graph): void {
  const n = graph.order;
  if (n === 0) return;

  // BFS to find connected components
  const visited = new Set<string>();
  const components: string[][] = [];

  graph.forEachNode((node) => {
    if (visited.has(node)) return;
    const component: string[] = [];
    const queue = [node];
    visited.add(node);
    while (queue.length > 0) {
      const current = queue.shift()!;
      component.push(current);
      graph.forEachNeighbor(current, (neighbor) => {
        if (!visited.has(neighbor)) {
          visited.add(neighbor);
          queue.push(neighbor);
        }
      });
    }
    components.push(component);
  });

  // Sort by size descending
  components.sort((a, b) => b.length - a.length);

  // Lay each component in a circle, pack them in a grid
  const cols = Math.max(1, Math.ceil(Math.sqrt(components.length)));
  const cellSize = Math.sqrt(n) * 5;

  for (let ci = 0; ci < components.length; ci++) {
    const comp = components[ci];
    const col = ci % cols;
    const row = Math.floor(ci / cols);
    const cx = col * cellSize;
    const cy = row * cellSize;

    if (comp.length === 1) {
      graph.setNodeAttribute(comp[0], 'x', cx);
      graph.setNodeAttribute(comp[0], 'y', cy);
      continue;
    }

    // Lay out in a circle scaled to component size
    const radius = Math.max(2, Math.sqrt(comp.length) * 2.5);
    for (let i = 0; i < comp.length; i++) {
      const angle = (2 * Math.PI * i) / comp.length;
      graph.setNodeAttribute(comp[i], 'x', cx + Math.cos(angle) * radius);
      graph.setNodeAttribute(comp[i], 'y', cy + Math.sin(angle) * radius);
    }
  }
}

/** Spiral — Archimedean spiral, high-degree nodes at center */
function layoutSpiral(graph: Graph): void {
  const n = graph.order;
  if (n === 0) return;

  // Sort by degree descending
  const sorted = graph.nodes().sort((a, b) => graph.degree(b) - graph.degree(a));

  // Vogel-disc spiral (golden angle) — fills a disc evenly
  const discRadius = Math.sqrt(n) * 3;
  const goldenAngle = Math.PI * (3 - Math.sqrt(5));

  for (let i = 0; i < sorted.length; i++) {
    const r = discRadius * Math.sqrt((i + 0.5) / n);
    const theta = i * goldenAngle;
    graph.setNodeAttribute(sorted[i], 'x', Math.cos(theta) * r);
    graph.setNodeAttribute(sorted[i], 'y', Math.sin(theta) * r);
  }
}

function applyLayout(graph: Graph, layout: string): void {
  switch (layout) {
    case 'circle': layoutCircle(graph); break;
    case 'cloud': layoutCloud(graph); break;
    case 'rings': layoutRings(graph); break;
    case 'lanes': layoutLanes(graph); break;
    case 'clusters': layoutClusters(graph); break;
    case 'islands': layoutIslands(graph); break;
    case 'spiral': layoutSpiral(graph); break;
    default: layoutCloud(graph); break;
  }
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

  selectedIdRef.current = selectedId;

  useEffect(() => {
    if (!containerRef.current || nodes.length === 0) return;

    const graph = new Graph();

    const visibleNodes = hiddenTypes.size > 0
      ? nodes.filter(n => !hiddenTypes.has(n.type))
      : nodes;
    const visibleNodeIds = new Set(visibleNodes.map(n => n.id));

    for (const node of visibleNodes) {
      const color = NODE_COLORS[node.type] || NODE_COLORS.entity;
      const size = Math.max(3, Math.min(12, 3 + Math.sqrt(node.edgeCount) * 0.6));
      graph.addNode(node.id, {
        label: node.label,
        x: Math.random() * 100,
        y: Math.random() * 100,
        size,
        color,
        nodeType: node.type,
      });
    }

    const edgeSet = new Set<string>();
    for (const edge of edges) {
      if (edge.sourceId === edge.targetId) continue;
      if (!visibleNodeIds.has(edge.sourceId) || !visibleNodeIds.has(edge.targetId)) continue;
      if (!graph.hasNode(edge.sourceId) || !graph.hasNode(edge.targetId)) continue;
      const key = `${edge.sourceId}-${edge.targetId}`;
      if (edgeSet.has(key)) continue;
      edgeSet.add(key);
      try {
        graph.addEdge(edge.sourceId, edge.targetId, {
          label: edge.label || '',
          size: 0.3,
          color: '#c8cdd3',
        });
      } catch { /* skip parallel */ }
    }

    applyLayout(graph, layout);

    graphRef.current = graph;
    if (sigmaRef.current) sigmaRef.current.kill();

    const sigma = new Sigma(graph, containerRef.current, {
      renderLabels: true,
      renderEdgeLabels: false,
      labelFont: 'Inter, system-ui, sans-serif',
      labelSize: 12,
      labelWeight: '500',
      labelColor: { color: '#94a3b8' },
      defaultNodeColor: '#78716c',
      defaultEdgeColor: '#c8cdd3',
      defaultEdgeType: 'curvedArrow',
      edgeProgramClasses: {
        curvedArrow: EdgeCurvedArrowProgram,
      },
      labelDensity: 0.07,
      labelGridCellSize: 100,
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

    const selectionOverlayCanvas = sigma.createCanvas('selectionEdges', {
      afterLayer: 'mouse',
      style: {
        pointerEvents: 'none',
      },
    });
    const selectionOverlayContext = selectionOverlayCanvas.getContext('2d');

    const syncSelectionOverlaySize = () => {
      const { width, height } = sigma.getDimensions();
      const pixelRatio = window.devicePixelRatio || 1;
      const nextWidth = Math.round(width * pixelRatio);
      const nextHeight = Math.round(height * pixelRatio);

      if (selectionOverlayCanvas.width !== nextWidth || selectionOverlayCanvas.height !== nextHeight) {
        selectionOverlayCanvas.width = nextWidth;
        selectionOverlayCanvas.height = nextHeight;
        selectionOverlayCanvas.style.width = `${width}px`;
        selectionOverlayCanvas.style.height = `${height}px`;
      }

      if (selectionOverlayContext) selectionOverlayContext.setTransform(pixelRatio, 0, 0, pixelRatio, 0, 0);

      return { width, height };
    };

    const renderSelectionOverlay = () => {
      if (!selectionOverlayContext) return;

      const { width, height } = syncSelectionOverlaySize();
      selectionOverlayContext.clearRect(0, 0, width, height);

      const currentSelectedId = selectedIdRef.current;
      const highlightedEdges = selectedEdgesRef.current;
      if (!currentSelectedId || highlightedEdges.length === 0 || !graph.hasNode(currentSelectedId)) return;

      selectionOverlayContext.save();
      selectionOverlayContext.lineCap = 'round';
      selectionOverlayContext.lineJoin = 'round';

      for (const edge of highlightedEdges) {
        if (!graph.hasNode(edge.sourceId) || !graph.hasNode(edge.targetId)) continue;

        const source = sigma.graphToViewport({
          x: graph.getNodeAttribute(edge.sourceId, 'x'),
          y: graph.getNodeAttribute(edge.sourceId, 'y'),
        });
        const target = sigma.graphToViewport({
          x: graph.getNodeAttribute(edge.targetId, 'x'),
          y: graph.getNodeAttribute(edge.targetId, 'y'),
        });

        const dx = target.x - source.x;
        const dy = target.y - source.y;
        const distance = Math.hypot(dx, dy) || 1;
        const perpendicularX = -dy / distance;
        const perpendicularY = dx / distance;
        const edgeKey = `${edge.sourceId}:${edge.targetId}:${edge.label}`;
        const direction = edgeKey.split('').reduce((hash, char) => hash + char.charCodeAt(0), 0) % 2 === 0 ? 1 : -1;
        const curvature = Math.max(16, Math.min(96, distance * 0.18));
        const control = {
          x: (source.x + target.x) / 2 + perpendicularX * curvature * direction,
          y: (source.y + target.y) / 2 + perpendicularY * curvature * direction,
        };

        selectionOverlayContext.beginPath();
        selectionOverlayContext.moveTo(source.x, source.y);
        selectionOverlayContext.quadraticCurveTo(control.x, control.y, target.x, target.y);
        selectionOverlayContext.strokeStyle = 'rgba(59, 130, 246, 0.18)';
        selectionOverlayContext.lineWidth = 5;
        selectionOverlayContext.stroke();

        selectionOverlayContext.beginPath();
        selectionOverlayContext.moveTo(source.x, source.y);
        selectionOverlayContext.quadraticCurveTo(control.x, control.y, target.x, target.y);
        selectionOverlayContext.strokeStyle = 'rgba(37, 99, 235, 0.98)';
        selectionOverlayContext.lineWidth = 1.6;
        selectionOverlayContext.stroke();

        const arrowAngle = Math.atan2(target.y - control.y, target.x - control.x);
        const arrowLength = Math.max(7, Math.min(11, distance * 0.04));
        const arrowSpread = Math.PI / 8;

        selectionOverlayContext.beginPath();
        selectionOverlayContext.moveTo(target.x, target.y);
        selectionOverlayContext.lineTo(
          target.x - Math.cos(arrowAngle - arrowSpread) * arrowLength,
          target.y - Math.sin(arrowAngle - arrowSpread) * arrowLength,
        );
        selectionOverlayContext.lineTo(
          target.x - Math.cos(arrowAngle + arrowSpread) * arrowLength,
          target.y - Math.sin(arrowAngle + arrowSpread) * arrowLength,
        );
        selectionOverlayContext.closePath();
        selectionOverlayContext.fillStyle = 'rgba(37, 99, 235, 0.98)';
        selectionOverlayContext.fill();
      }

      selectionOverlayContext.restore();
    };

    sigma.on('afterRender', renderSelectionOverlay);
    selectionOverlayRenderRef.current = renderSelectionOverlay;
    renderSelectionOverlay();

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

    return () => {
      container.removeEventListener('wheel', wheelHandler);
      sigma.off('afterRender', renderSelectionOverlay);
      selectionOverlayRenderRef.current = null;
      sigma.killLayer('selectionEdges');
      sigma.kill();
      sigmaRef.current = null;
    };
  }, [nodes, edges, layout, onSelect, hiddenTypes]);

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
        return {
          ...data,
          color: isSelected || isNeighbor ? data.color : '#d4d4d8',
          zIndex: isSelected ? 2 : isNeighbor ? 1 : 0,
          label: isSelected || isNeighbor ? data.label : '',
        };
      });

      sigma.setSetting('edgeReducer', (edge: string, data: any) => {
        const source = graph.source(edge);
        const target = graph.target(edge);
        const isConnected = source === selectedId || target === selectedId;
        return {
          ...data,
          color: isConnected ? 'rgba(59, 130, 246, 0.34)' : '#f0f0f0',
          size: isConnected ? 0.4 : 0.15,
          zIndex: isConnected ? 2 : 0,
          hidden: false,
        };
      });
    } else {
      selectedEdgesRef.current = [];
      sigma.setSetting('nodeReducer', null);
      sigma.setSetting('edgeReducer', null);
    }

    sigma.refresh();
    selectionOverlayRenderRef.current?.();
  }, [selectedId, edges, nodes, layout, hiddenTypes]);

  return (
    <div ref={containerRef} className="w-full h-full" style={{ minHeight: '400px' }} />
  );
}
