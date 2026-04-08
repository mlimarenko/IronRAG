import { useEffect, useRef } from 'react';
import Graph from 'graphology';
import Sigma from 'sigma';
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

export default function SigmaGraph({ nodes, edges, selectedId, onSelect, layout, hiddenTypes }: SigmaGraphProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const sigmaRef = useRef<Sigma | null>(null);
  const graphRef = useRef<Graph | null>(null);

  // Build graph when data or filter changes
  useEffect(() => {
    if (!containerRef.current || nodes.length === 0) return;

    const graph = new Graph();

    // Filter by hidden types
    const visibleNodes = hiddenTypes.size > 0
      ? nodes.filter(n => !hiddenTypes.has(n.type))
      : nodes;

    const visibleNodeIds = new Set(visibleNodes.map(n => n.id));

    for (const node of visibleNodes) {
      const color = NODE_COLORS[node.type] || NODE_COLORS.entity;
      const size = Math.max(2, Math.min(15, 2 + Math.sqrt(node.edgeCount) * 0.8));
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
          size: 1,
          color: 'rgba(148, 163, 184, 0.35)',
        });
      } catch {
        // Skip parallel edges
      }
    }

    if (layout === 'circle') {
      circular.assign(graph);
    } else {
      circular.assign(graph);
      graph.forEachNode((node) => {
        const attrs = graph.getNodeAttributes(node);
        graph.setNodeAttribute(node, 'x', attrs.x + (Math.random() - 0.5) * 10);
        graph.setNodeAttribute(node, 'y', attrs.y + (Math.random() - 0.5) * 10);
      });
    }

    graphRef.current = graph;

    if (sigmaRef.current) {
      sigmaRef.current.kill();
    }

    const sigma = new Sigma(graph, containerRef.current, {
      renderLabels: true,
      renderEdgeLabels: false,
      labelFont: 'Inter, system-ui, sans-serif',
      labelSize: 12,
      labelWeight: '500',
      labelColor: { color: '#94a3b8' },
      defaultNodeColor: '#78716c',
      defaultEdgeColor: 'rgba(148, 163, 184, 0.35)',
      defaultEdgeType: 'line',
      labelDensity: 0.07,
      labelGridCellSize: 100,
      zIndex: true,
      minCameraRatio: 0.01,
      maxCameraRatio: 50,
      zoomToSizeRatioFunction: (x: number) => x,
    });

    // Faster zoom: override wheel handler sensitivity
    const camera = sigma.getCamera();
    containerRef.current.addEventListener('wheel', (e) => {
      e.preventDefault();
      const factor = e.deltaY > 0 ? 1.15 : 0.85; // faster zoom steps
      const newRatio = camera.ratio * factor;
      camera.animate({ ratio: Math.max(0.01, Math.min(50, newRatio)) }, { duration: 50 });
    }, { passive: false });

    sigma.on('clickNode', ({ node }) => {
      onSelect(node);
    });
    sigma.on('clickStage', () => {
      onSelect(null);
    });

    sigmaRef.current = sigma;

    return () => {
      sigma.kill();
      sigmaRef.current = null;
    };
  }, [nodes, edges, layout, onSelect, hiddenTypes]);

  // Handle selection highlighting
  useEffect(() => {
    const sigma = sigmaRef.current;
    const graph = graphRef.current;
    if (!sigma || !graph) return;

    graph.forEachNode((node) => {
      const type = graph.getNodeAttribute(node, 'nodeType') as string;
      const baseColor = NODE_COLORS[type] || NODE_COLORS.entity;
      if (selectedId) {
        const isSelected = node === selectedId;
        const isNeighbor = graph.hasNode(selectedId) && graph.areNeighbors(node, selectedId);
        graph.setNodeAttribute(node, 'color', isSelected || isNeighbor ? baseColor : baseColor + '25');
        graph.setNodeAttribute(node, 'zIndex', isSelected ? 2 : isNeighbor ? 1 : 0);
      } else {
        graph.setNodeAttribute(node, 'color', baseColor);
        graph.setNodeAttribute(node, 'zIndex', 0);
      }
    });

    graph.forEachEdge((edge) => {
      const source = graph.source(edge);
      const target = graph.target(edge);
      if (selectedId && (source === selectedId || target === selectedId)) {
        graph.setEdgeAttribute(edge, 'color', 'rgba(59, 130, 246, 0.85)');
        graph.setEdgeAttribute(edge, 'size', 2.5);
      } else {
        graph.setEdgeAttribute(edge, 'color', selectedId ? 'rgba(148, 163, 184, 0.06)' : 'rgba(148, 163, 184, 0.35)');
        graph.setEdgeAttribute(edge, 'size', 1);
      }
    });

    sigma.refresh();
  }, [selectedId]);

  return (
    <div ref={containerRef} className="w-full h-full" style={{ minHeight: '400px' }} />
  );
}
