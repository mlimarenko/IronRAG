import { useState, useEffect, useRef, useCallback, useMemo, type PointerEvent as ReactPointerEvent } from 'react';
import { useTranslation } from 'react-i18next';
import { toast } from 'sonner';
import { useApp } from '@/contexts/AppContext';
import { useNavigate } from 'react-router-dom';
import { knowledgeApi } from '@/api';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select';
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip';
import {
  Search, ZoomIn, ZoomOut, Maximize2, X, Filter, Loader2,
  FileText, Share2, AlertTriangle, Eye, EyeOff, ExternalLink
} from 'lucide-react';
import type { GraphNode, GraphNodeType, GraphMetadata, GraphStatus } from '@/types';

const LAYOUTS = ['force', 'cloud', 'circle', 'rings', 'lanes', 'clusters', 'islands', 'spiral'] as const;
type LayoutType = typeof LAYOUTS[number];

const NODE_COLORS: Record<GraphNodeType, string> = {
  document: 'hsl(var(--status-processing))',
  entity: 'hsl(var(--status-ready))',
  topic: 'hsl(var(--status-warning))',
};

function scaledRadii(total: number): Record<GraphNodeType, { min: number; max: number }> {
  // Scale node sizes down as count grows — prevent overlapping
  const s = total > 100 ? 0.4 : total > 50 ? 0.6 : total > 20 ? 0.8 : 1.0;
  return {
    document: { min: 1.5 * s, max: 2.8 * s },
    entity: { min: 1.0 * s, max: 1.8 * s },
    topic: { min: 0.7 * s, max: 1.3 * s },
  };
}

// ---------------------------------------------------------------------------
// Force-directed layout simulation
// ---------------------------------------------------------------------------

interface ForceNode { id: string; x: number; y: number; vx: number; vy: number }

function forceLayout(
  nodeIds: string[],
  edges: EdgeData[],
  width: number,
  height: number,
  iterations = 120,
): Map<string, { x: number; y: number }> {
  const n = nodeIds.length;
  const result = new Map<string, { x: number; y: number }>();
  if (n === 0) return result;

  const nodes: ForceNode[] = nodeIds.map((id, i) => {
    const angle = (i / n) * Math.PI * 2;
    const initR = Math.min(width, height) * 0.35;
    return {
      id,
      x: width / 2 + initR * Math.cos(angle) + (Math.random() - 0.5) * 2,
      y: height / 2 + initR * Math.sin(angle) + (Math.random() - 0.5) * 2,
      vx: 0,
      vy: 0,
    };
  });

  const idxMap = new Map<string, number>();
  nodes.forEach((nd, i) => idxMap.set(nd.id, i));

  const cx = width / 2;
  const cy = height / 2;
  const repulsionStrength = 3000 + n * 40;
  const attractionStrength = 0.004;
  const centeringStrength = 0.003;
  const damping = 0.78;
  const maxSpeed = 6;

  for (let iter = 0; iter < iterations; iter++) {
    const temp = 1 - iter / iterations;

    // Repulsion (Coulomb)
    for (let i = 0; i < n; i++) {
      for (let j = i + 1; j < n; j++) {
        let dx = nodes[i].x - nodes[j].x;
        let dy = nodes[i].y - nodes[j].y;
        let dist = Math.sqrt(dx * dx + dy * dy);
        if (dist < 0.5) { dist = 0.5; dx = Math.random() - 0.5; dy = Math.random() - 0.5; }
        const force = (repulsionStrength * temp) / (dist * dist);
        const fx = (dx / dist) * force;
        const fy = (dy / dist) * force;
        nodes[i].vx += fx; nodes[i].vy += fy;
        nodes[j].vx -= fx; nodes[j].vy -= fy;
      }
    }

    // Attraction along edges (Hooke)
    for (const edge of edges) {
      const si = idxMap.get(edge.sourceId);
      const ti = idxMap.get(edge.targetId);
      if (si == null || ti == null) continue;
      const dx = nodes[ti].x - nodes[si].x;
      const dy = nodes[ti].y - nodes[si].y;
      const dist = Math.sqrt(dx * dx + dy * dy);
      if (dist < 0.1) continue;
      const force = dist * attractionStrength;
      const fx = (dx / dist) * force;
      const fy = (dy / dist) * force;
      nodes[si].vx += fx; nodes[si].vy += fy;
      nodes[ti].vx -= fx; nodes[ti].vy -= fy;
    }

    // Centering
    for (let i = 0; i < n; i++) {
      nodes[i].vx += (cx - nodes[i].x) * centeringStrength;
      nodes[i].vy += (cy - nodes[i].y) * centeringStrength;
    }

    // Apply velocity
    for (let i = 0; i < n; i++) {
      nodes[i].vx *= damping;
      nodes[i].vy *= damping;
      const speed = Math.sqrt(nodes[i].vx * nodes[i].vx + nodes[i].vy * nodes[i].vy);
      if (speed > maxSpeed) {
        nodes[i].vx = (nodes[i].vx / speed) * maxSpeed;
        nodes[i].vy = (nodes[i].vy / speed) * maxSpeed;
      }
      nodes[i].x += nodes[i].vx;
      nodes[i].y += nodes[i].vy;
    }
  }

  // Normalize into [padding, width-padding]
  const padding = 8;
  let minX = Infinity, maxX = -Infinity, minY = Infinity, maxY = -Infinity;
  for (const nd of nodes) {
    if (nd.x < minX) minX = nd.x;
    if (nd.x > maxX) maxX = nd.x;
    if (nd.y < minY) minY = nd.y;
    if (nd.y > maxY) maxY = nd.y;
  }
  const rangeX = maxX - minX || 1;
  const rangeY = maxY - minY || 1;
  for (const nd of nodes) {
    result.set(nd.id, {
      x: padding + ((nd.x - minX) / rangeX) * (width - 2 * padding),
      y: padding + ((nd.y - minY) / rangeY) * (height - 2 * padding),
    });
  }
  return result;
}

/** Map backend GraphWorkbenchSurface to frontend GraphNode[] + GraphMetadata */
function mapWorkbenchToUI(workbench: any): { nodes: GraphNode[]; meta: GraphMetadata; recommendedLayout?: string; selectedDetail: any | null } {
  const graph = workbench.graph ?? {};
  const nodes: GraphNode[] = (graph.nodes ?? []).map((n: any) => ({
    id: n.id,
    label: n.label,
    type: mapNodeType(n.nodeType),
    summary: n.summary ?? n.secondaryLabel ?? undefined,
    edgeCount: n.supportCount ?? 0,
    properties: {},
    sourceDocumentIds: [],
  }));

  const meta: GraphMetadata = {
    nodeCount: graph.nodeCount ?? nodes.length,
    edgeCount: graph.edgeCount ?? 0,
    hiddenDisconnectedCount: 0,
    status: mapStatus(graph.status),
    convergenceStatus: graph.convergenceStatus ?? 'current',
    recommendedLayout: undefined,
  };

  return { nodes, meta, selectedDetail: workbench.selectedNode ?? null };
}

function mapNodeType(t: string | undefined): GraphNodeType {
  if (t === 'entity') return 'entity';
  if (t === 'topic') return 'topic';
  if (t === 'document') return 'document';
  return 'entity';
}

function mapStatus(s: string | undefined): GraphStatus {
  if (s === 'empty' || s === 'building' || s === 'rebuilding' || s === 'ready' || s === 'partial' || s === 'failed' || s === 'stale') return s as GraphStatus;
  return 'ready';
}

/** Map backend entity detail to enriched GraphNode for the inspector */
function mapEntityDetailToNode(detail: any): GraphNode {
  const props: Record<string, string> = {};
  if (detail.properties) {
    for (const [k, v] of detail.properties) {
      props[k] = String(v);
    }
  }
  return {
    id: detail.id,
    label: detail.label,
    type: mapNodeType(detail.nodeType),
    summary: detail.summary,
    edgeCount: detail.relatedNodes?.length ?? 0,
    properties: props,
    sourceDocumentIds: (detail.supportingDocuments ?? []).map((d: any) => d.documentId),
  };
}

interface EdgeData { id: string; sourceId: string; targetId: string; label: string; weight: number }

/** Compute a quadratic bezier control point offset perpendicular to the edge midpoint */
function curvedEdgePath(x1: number, y1: number, x2: number, y2: number): string {
  const mx = (x1 + x2) / 2;
  const my = (y1 + y2) / 2;
  const dx = x2 - x1;
  const dy = y2 - y1;
  const dist = Math.sqrt(dx * dx + dy * dy);
  // Perpendicular offset proportional to distance
  const offset = dist * 0.15;
  // Normal direction (rotate 90 degrees)
  const nx = -dy / (dist || 1);
  const ny = dx / (dist || 1);
  const cx = mx + nx * offset;
  const cy = my + ny * offset;
  return `M ${x1} ${y1} Q ${cx} ${cy} ${x2} ${y2}`;
}

function GraphCanvas({ nodes, edges, selectedId, onSelect, layout, zoom, onZoomChange, fitKey }: {
  nodes: GraphNode[]; edges: EdgeData[]; selectedId: string | null; onSelect: (id: string | null) => void; layout: LayoutType; zoom: number; onZoomChange: (z: number) => void; fitKey: number;
}) {
  const canvasRef = useRef<HTMLDivElement>(null);
  const svgRef = useRef<SVGSVGElement>(null);
  const dragStartRef = useRef({ x: 0, y: 0 });
  const panStartRef = useRef({ x: 0, y: 0 });
  const draggedRef = useRef(false);
  const [pan, setPan] = useState({ x: 0, y: 0 });
  const [isDragging, setIsDragging] = useState(false);
  const isPanningRef = useRef(false);

  // Node dragging state
  const [dragNodeId, setDragNodeId] = useState<string | null>(null);
  const hoveredNodeRef = useRef<string | null>(null);
  const [hoveredNodeId, setHoveredNodeId] = useState<string | null>(null);
  const hoverTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [nodePositions, setNodePositions] = useState<Map<string, { x: number; y: number }>>(new Map());
  const dragNodeRef = useRef<string | null>(null);

  // Memoize viewBox
  const viewBox = useMemo(() => {
    const sz = Math.max(100, Math.ceil(Math.sqrt(nodes.length) * 14));
    return `0 0 ${sz} ${sz}`;
  }, [nodes.length]);

  // Pre-compute node index map for O(1) position lookups
  const nodeIndexMap = useMemo(() => {
    const m = new Map<string, number>();
    nodes.forEach((n, i) => m.set(n.id, i));
    return m;
  }, [nodes]);

  // Pre-compute adjacency map: nodeId → Set of connected nodeIds (O(E) once, then O(1) lookup)
  const adjacency = useMemo(() => {
    const adj = new Map<string, Set<string>>();
    for (const e of edges) {
      if (!adj.has(e.sourceId)) adj.set(e.sourceId, new Set());
      if (!adj.has(e.targetId)) adj.set(e.targetId, new Set());
      adj.get(e.sourceId)!.add(e.targetId);
      adj.get(e.targetId)!.add(e.sourceId);
    }
    return adj;
  }, [edges]);

  // Pre-compute label visibility threshold
  const labelConfig = useMemo(() => {
    const n = nodes.length;
    const maxChars = n > 120 ? 10 : n > 60 ? 13 : n > 30 ? 16 : 20;
    const fontSize = n > 120 ? 1.8 : n > 60 ? 2.0 : n > 30 ? 2.2 : 2.5;
    const labelBudget = Math.max(15, Math.min(n, Math.ceil(n * 0.3)));
    const sorted = nodes.map(nd => nd.edgeCount).sort((a, b) => b - a);
    const edgeThreshold = sorted[Math.min(labelBudget, sorted.length - 1)] ?? 0;
    const topNodeIds = new Set(nodes.filter(nd => nd.edgeCount >= edgeThreshold || nd.type === 'document').map(nd => nd.id));
    return { maxChars, fontSize, topNodeIds };
  }, [nodes]);

  // Pre-compute cluster/island metadata to avoid O(N²) in getPosition
  const clusterMeta = useMemo(() => {
    const typeGroup: Record<string, number> = { document: 0, entity: 1, topic: 2 };
    const clusterSizes: Record<number, number> = {};
    const clusterCounters: Record<number, number> = {};
    const nodeClusterIndex: number[] = [];
    for (const node of nodes) {
      const g = typeGroup[node.type] ?? 1;
      clusterSizes[g] = (clusterSizes[g] ?? 0) + 1;
    }
    for (const node of nodes) {
      const g = typeGroup[node.type] ?? 1;
      const ci = clusterCounters[g] ?? 0;
      nodeClusterIndex.push(ci);
      clusterCounters[g] = ci + 1;
    }
    return { typeGroup, clusterSizes, nodeClusterIndex };
  }, [nodes]);

  // Scale node sizes based on total count
  const radii = useMemo(() => scaledRadii(nodes.length), [nodes.length]);
  const nodeRadius = useCallback((node: GraphNode) => {
    const range = radii[node.type];
    return Math.max(range.min, Math.min(range.max, range.min + node.edgeCount * 0.1));
  }, [radii]);

  // Debounced hover — update state only after pointer rests 50ms to avoid re-render storms
  const handleNodeEnter = useCallback((nodeId: string) => {
    hoveredNodeRef.current = nodeId;
    if (hoverTimerRef.current) clearTimeout(hoverTimerRef.current);
    hoverTimerRef.current = setTimeout(() => {
      if (hoveredNodeRef.current === nodeId) setHoveredNodeId(nodeId);
    }, 50);
  }, []);
  const handleNodeLeave = useCallback((nodeId: string) => {
    if (hoveredNodeRef.current === nodeId) hoveredNodeRef.current = null;
    if (hoverTimerRef.current) clearTimeout(hoverTimerRef.current);
    hoverTimerRef.current = setTimeout(() => {
      if (hoveredNodeRef.current === null) setHoveredNodeId(null);
    }, 50);
  }, []);

  // Compute force layout positions when layout === 'force' or recompute on node/edge changes
  const forcePositionsRef = useRef<Map<string, { x: number; y: number }>>(new Map());

  useEffect(() => {
    if (layout === 'force' && nodes.length > 0) {
      const sz = Math.max(100, Math.ceil(Math.sqrt(nodes.length) * 14));
      const iters = nodes.length > 80 ? 180 : 120;
      const positions = forceLayout(nodes.map(n => n.id), edges, sz, sz, iters);
      forcePositionsRef.current = positions;
      setNodePositions(new Map(positions));
    } else {
      forcePositionsRef.current = new Map();
      setNodePositions(new Map());
    }
  }, [nodes, edges, layout]);

  useEffect(() => {
    setPan({ x: 0, y: 0 });
  }, [fitKey]);

  useEffect(() => {
    const el = canvasRef.current;
    if (!el) return;
    const handler = (e: WheelEvent) => {
      e.preventDefault();
      const delta = e.deltaY > 0 ? -0.1 : 0.1;
      onZoomChange(Math.min(20, Math.max(0.1, zoom + delta)));
    };
    el.addEventListener('wheel', handler, { passive: false });
    return () => el.removeEventListener('wheel', handler);
  }, [zoom, onZoomChange]);

  /** Convert a client (screen) coordinate to SVG viewBox coordinate */
  const clientToSvg = useCallback((clientX: number, clientY: number): { x: number; y: number } => {
    const svg = svgRef.current;
    if (!svg) return { x: 0, y: 0 };
    const pt = svg.createSVGPoint();
    pt.x = clientX;
    pt.y = clientY;
    const ctm = svg.getScreenCTM();
    if (!ctm) return { x: 0, y: 0 };
    const svgPt = pt.matrixTransform(ctm.inverse());
    return { x: svgPt.x, y: svgPt.y };
  }, []);

  const handlePointerDown = (e: ReactPointerEvent<HTMLDivElement>) => {
    if (e.button !== 0) return;
    // If a node drag is active, ignore canvas panning
    if (dragNodeRef.current) return;
    draggedRef.current = false;
    dragStartRef.current = { x: e.clientX, y: e.clientY };
    panStartRef.current = pan;
    isPanningRef.current = true;
  };

  const handlePointerMove = (e: ReactPointerEvent<HTMLDivElement>) => {
    // Node dragging takes priority
    if (dragNodeRef.current) {
      const svgPos = clientToSvg(e.clientX, e.clientY);
      setNodePositions(prev => {
        const next = new Map(prev);
        next.set(dragNodeRef.current!, { x: svgPos.x, y: svgPos.y });
        return next;
      });
      draggedRef.current = true;
      return;
    }

    if (!isPanningRef.current) return;
    const deltaX = e.clientX - dragStartRef.current.x;
    const deltaY = e.clientY - dragStartRef.current.y;
    if (Math.abs(deltaX) > 3 || Math.abs(deltaY) > 3) {
      draggedRef.current = true;
      setIsDragging(true);
    }
    if (draggedRef.current) {
      setPan({ x: panStartRef.current.x + deltaX, y: panStartRef.current.y + deltaY });
    }
  };

  const handlePointerUp = (_e: ReactPointerEvent<HTMLDivElement>) => {
    if (dragNodeRef.current) {
      dragNodeRef.current = null;
      setDragNodeId(null);
      // Keep draggedRef true so the click handler knows not to select
      return;
    }
    isPanningRef.current = false;
    setIsDragging(false);
  };

  const handleCanvasClick = () => {
    if (draggedRef.current) {
      draggedRef.current = false;
      return;
    }
    onSelect(null);
  };

  const handleNodePointerDown = useCallback((e: React.PointerEvent, nodeId: string) => {
    e.stopPropagation();
    e.preventDefault();
    dragNodeRef.current = nodeId;
    setDragNodeId(nodeId);
    draggedRef.current = false;
    // Initialize position if not already in the map (for non-force layouts)
    setNodePositions(prev => {
      if (prev.has(nodeId)) return prev;
      // We need a position -- get it from the layout
      return prev;
    });
  }, []);

  const getPosition = useCallback((index: number, total: number, nodeId?: string) => {
    // If we have an override position for this node (from dragging or force layout), use it
    if (nodeId && nodePositions.has(nodeId)) {
      return nodePositions.get(nodeId)!;
    }

    // Use viewBox-aware coordinates
    const sz = Math.max(100, Math.ceil(Math.sqrt(total) * 14));
    const margin = sz * 0.06;
    const usable = sz - margin * 2;
    const cx = sz / 2, cy = sz / 2;

    switch (layout) {
      case 'force': {
        // Fallback initial positions for force — actual positions come from forceLayout
        const angle = (index / total) * Math.PI * 2 - Math.PI / 2;
        return { x: cx + (usable * 0.4) * Math.cos(angle), y: cy + (usable * 0.4) * Math.sin(angle) };
      }
      case 'circle': {
        const angle = (index / total) * Math.PI * 2 - Math.PI / 2;
        return { x: cx + (usable * 0.45) * Math.cos(angle), y: cy + (usable * 0.45) * Math.sin(angle) };
      }
      case 'spiral': {
        const turns = Math.max(3, Math.ceil(total / 15));
        const angle = (index / total) * Math.PI * 2 * turns;
        const r = usable * 0.03 + (index / total) * usable * 0.45;
        return { x: cx + r * Math.cos(angle), y: cy + r * Math.sin(angle) };
      }
      case 'rings': {
        const ringCount = Math.max(2, Math.ceil(Math.sqrt(total / 8)));
        const ring = Math.min(Math.floor(index / Math.ceil(total / ringCount)), ringCount - 1);
        const nodesInRing = Math.ceil(total / ringCount);
        const ringIndex = index - ring * nodesInRing;
        const angle = (ringIndex / nodesInRing) * Math.PI * 2;
        const r = usable * 0.1 + (ring / (ringCount - 1 || 1)) * usable * 0.38;
        return { x: cx + r * Math.cos(angle), y: cy + r * Math.sin(angle) };
      }
      case 'lanes': {
        const cols = Math.max(4, Math.ceil(Math.sqrt(total * 1.5)));
        const rows = Math.ceil(total / cols);
        const colW = usable / cols;
        const rowH = usable / Math.max(rows, 1);
        const row = Math.floor(index / cols);
        const col = index % cols;
        return { x: margin + col * colW + colW / 2, y: margin + row * rowH + rowH / 2 };
      }
      case 'clusters':
      case 'islands': {
        // Use precomputed cluster data (built in clusterMeta memo)
        const meta = clusterMeta;
        const nodeType = nodes[index]?.type ?? 'entity';
        const group = meta.typeGroup[nodeType] ?? 1;
        const ci = meta.nodeClusterIndex[index] ?? 0;
        const ct = meta.clusterSizes[group] ?? 1;
        const isIslands = layout === 'islands';
        const centers = isIslands
          ? [{ x: cx, y: margin + usable * 0.15 }, { x: margin + usable * 0.2, y: cy + usable * 0.2 }, { x: cx + usable * 0.25, y: cy + usable * 0.2 }]
          : [{ x: cx - usable * 0.25, y: cy - usable * 0.2 }, { x: cx + usable * 0.2, y: cy }, { x: cx - usable * 0.1, y: cy + usable * 0.25 }];
        const cc = centers[group] ?? centers[1];
        if (isIslands) {
          const spiralAngle = (ci / Math.max(ct, 1)) * Math.PI * 6;
          const r = 2 + (ci / Math.max(ct, 1)) * usable * 0.2;
          return { x: cc.x + r * Math.cos(spiralAngle), y: cc.y + r * Math.sin(spiralAngle) };
        } else {
          const angle = (ci / Math.max(ct, 1)) * Math.PI * 2;
          const r = Math.min(usable * 0.22, 3 + Math.sqrt(ct) * usable * 0.015);
          return { x: cc.x + r * Math.cos(angle), y: cc.y + r * Math.sin(angle) };
        }
      }
      default: { // cloud
        const seed = index * 137.508;
        const r = usable * 0.06 + (index / total) * usable * 0.4;
        return { x: cx + r * Math.cos(seed) + (Math.sin(seed * 2) * usable * 0.03), y: cy + r * Math.sin(seed) + (Math.cos(seed * 3) * usable * 0.03) };
      }
    }
  }, [layout, nodePositions]);

  // Reset camera when layout changes
  useEffect(() => {
    setPan({ x: 0, y: 0 });
  }, [layout]);

  // On first render for non-force layouts, seed nodePositions so dragging works
  useEffect(() => {
    if (layout === 'force') return; // force layout sets positions itself
    if (nodes.length === 0) return;
    const initial = new Map<string, { x: number; y: number }>();
    nodes.forEach((node, i) => {
      const pos = getPosition(i, nodes.length);
      initial.set(node.id, pos);
    });
    setNodePositions(initial);
    // Only run when layout or node list identity changes, not when getPosition ref changes during drag
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [layout, nodes]);

  return (
    <div
      ref={canvasRef}
      className={`w-full h-full relative overflow-hidden ${dragNodeId ? 'cursor-grabbing' : isDragging ? 'cursor-grabbing' : 'cursor-grab'}`}
      onClick={handleCanvasClick}
      onPointerDown={handlePointerDown}
      onPointerMove={handlePointerMove}
      onPointerUp={handlePointerUp}
      onPointerCancel={handlePointerUp}
      style={{ touchAction: 'none' }}
    >
      <svg
        ref={svgRef}
        className="w-full h-full"
        viewBox={viewBox}
        preserveAspectRatio="xMidYMid meet"
        style={{ transform: `translate(${pan.x}px, ${pan.y}px) scale(${zoom})`, transformOrigin: 'center', transition: (isDragging || dragNodeId) ? 'none' : 'transform 0.3s cubic-bezier(0.16, 1, 0.3, 1)' }}
      >
        <defs>
          {/* Dot grid pattern */}
          <pattern id="dot-grid" width="5" height="5" patternUnits="userSpaceOnUse">
            <circle cx="2.5" cy="2.5" r="0.15" fill="currentColor" opacity="0.12" />
          </pattern>
          <radialGradient id="glow-doc" cx="50%" cy="50%" r="50%"><stop offset="0%" stopColor="hsl(var(--status-processing))" stopOpacity="0.3"/><stop offset="100%" stopColor="hsl(var(--status-processing))" stopOpacity="0"/></radialGradient>
          <radialGradient id="glow-entity" cx="50%" cy="50%" r="50%"><stop offset="0%" stopColor="hsl(var(--status-ready))" stopOpacity="0.3"/><stop offset="100%" stopColor="hsl(var(--status-ready))" stopOpacity="0"/></radialGradient>
          <radialGradient id="glow-topic" cx="50%" cy="50%" r="50%"><stop offset="0%" stopColor="hsl(var(--status-warning))" stopOpacity="0.3"/><stop offset="100%" stopColor="hsl(var(--status-warning))" stopOpacity="0"/></radialGradient>
        </defs>

        {/* Grid background — oversized to cover panning/zooming */}
        <rect x="-500" y="-500" width="2000" height="2000" fill="url(#dot-grid)" />

        {/* Edges -- curved bezier paths (only between visible nodes) */}
        {edges.map(edge => {
          const si = nodeIndexMap.get(edge.sourceId);
          const ti = nodeIndexMap.get(edge.targetId);
          if (si == null || ti == null) return null;
          const sp = getPosition(si, nodes.length, edge.sourceId);
          const tp = getPosition(ti, nodes.length, edge.targetId);
          const isConnected = selectedId === edge.sourceId || selectedId === edge.targetId;
          // Scale stroke width with viewBox (larger graphs need thicker lines to be visible)
          const baseStroke = Math.max(0.15, parseInt(viewBox.split(' ')[2]) * 0.0004);
          return (
            <path
              key={`e-${edge.id}`}
              d={curvedEdgePath(sp.x, sp.y, tp.x, tp.y)}
              stroke={isConnected ? 'hsl(var(--primary))' : 'hsl(var(--muted-foreground))'}
              strokeWidth={isConnected ? baseStroke * 2.5 : baseStroke}
              opacity={selectedId ? (isConnected ? 0.9 : 0.1) : 0.3}
              fill="none"
            />
          );
        })}

        {/* Nodes */}
        {nodes.map((node, i) => {
          const pos = getPosition(i, nodes.length, node.id);
          const isSelected = selectedId === node.id;
          const size = nodeRadius(node);
          const isBeingDragged = dragNodeId === node.id;
          // O(1) adjacency check instead of O(E) edges.some()
          const isConnectedToSelected = selectedId ? (adjacency.get(selectedId)?.has(node.id) ?? false) : false;
          const dimmed = selectedId && !isSelected && !isConnectedToSelected;
          const isHovered = hoveredNodeId === node.id;
          const showLabel = isSelected || isConnectedToSelected || isHovered || labelConfig.topNodeIds.has(node.id);
          const text = node.label.length > labelConfig.maxChars ? node.label.slice(0, labelConfig.maxChars - 1) + '\u2026' : node.label;

          return (
            <g
              key={node.id}
              onPointerDown={e => handleNodePointerDown(e, node.id)}
              onPointerEnter={() => handleNodeEnter(node.id)}
              onPointerLeave={() => handleNodeLeave(node.id)}
              onClick={e => { e.stopPropagation(); if (draggedRef.current) { draggedRef.current = false; return; } onSelect(node.id); }}
              className="cursor-pointer"
            >
              {isSelected && <circle cx={pos.x} cy={pos.y} r={size * 1.8} fill={`url(#glow-${node.type === 'document' ? 'doc' : node.type})`} />}
              <circle
                cx={pos.x} cy={pos.y} r={size}
                fill={NODE_COLORS[node.type]}
                stroke={isSelected ? 'hsl(var(--foreground))' : 'transparent'}
                strokeWidth={isSelected ? size * 0.12 : 0}
                opacity={dimmed ? 0.15 : 0.9}
              />
              {showLabel && (
                <text
                  x={pos.x} y={pos.y + size + labelConfig.fontSize + 0.5}
                  textAnchor="middle" fontSize={labelConfig.fontSize}
                  fill="hsl(var(--muted-foreground))"
                  className="select-none pointer-events-none"
                  fontWeight={isSelected ? '700' : isConnectedToSelected || isHovered ? '600' : '500'}
                  opacity={dimmed ? 0.15 : isSelected || isHovered ? 1 : 0.75}
                >
                  {text}
                </text>
              )}
            </g>
          );
        })}
      </svg>
    </div>
  );
}

export default function GraphPage() {
  const { t } = useTranslation();
  const { activeLibrary } = useApp();
  const navigate = useNavigate();

  // Edges from entities/relations fallback
  const edgesRef = useRef<{ id: string; sourceId: string; targetId: string; label: string; weight: number }[]>([]);

  // API state
  const [allNodes, setAllNodes] = useState<GraphNode[]>([]);
  const [graphMeta, setGraphMeta] = useState<GraphMetadata | null>(null);
  const [graphStatus, setGraphStatus] = useState<GraphStatus>('building');
  const [loadError, setLoadError] = useState<string | null>(null);

  // Node detail state (from entity detail endpoint)
  const [selectedNode, setSelectedNode] = useState<string | null>(null);
  const [selectedDetail, setSelectedDetail] = useState<GraphNode | null>(null);
  const [detailLoading, setDetailLoading] = useState(false);

  // UI controls
  const [searchQuery, setSearchQuery] = useState('');
  const [nodeTypeFilter, setNodeTypeFilter] = useState<'all' | GraphNodeType>('all');
  const [layout, setLayout] = useState<LayoutType>('force');
  const [zoom, setZoom] = useState(1);
  const [showFiltered, setShowFiltered] = useState(true);
  const [fitKey, setFitKey] = useState(0);

  // Fetch graph workbench data, falling back to entities+relations endpoints
  useEffect(() => {
    if (!activeLibrary) return;
    let cancelled = false;
    setGraphStatus('building');
    setLoadError(null);
    setAllNodes([]);
    setGraphMeta(null);
    setSelectedNode(null);
    setSelectedDetail(null);

    knowledgeApi.getGraphWorkbench(activeLibrary.id)
      .then(async workbench => {
        if (cancelled) return;
        const { nodes, meta } = mapWorkbenchToUI(workbench);

        // If workbench returned no nodes, fall back to entities + relations endpoints
        if (nodes.length === 0) {
          const [entitiesRes, relationsRes, documentsRes, topologyRes] = await Promise.all([
            knowledgeApi.listEntities(activeLibrary.id),
            knowledgeApi.listRelations(activeLibrary.id),
            knowledgeApi.listDocuments(activeLibrary.id),
            knowledgeApi.getGraphTopology(activeLibrary.id).catch(() => null),
          ]);
          if (cancelled) return;

          const entities: any[] = Array.isArray(entitiesRes) ? entitiesRes : (entitiesRes.items ?? []);
          const relations: any[] = Array.isArray(relationsRes) ? relationsRes : (relationsRes.items ?? []);
          const documents: any[] = Array.isArray(documentsRes) ? documentsRes : (documentsRes.items ?? documentsRes.documents ?? []);
          const documentLinks: any[] = topologyRes?.documentLinks ?? [];

          // Pre-compute per-document edge counts from topology links
          const docEdgeCounts = new Map<string, number>();
          documentLinks.forEach((link: any) => {
            docEdgeCounts.set(link.documentId, (docEdgeCounts.get(link.documentId) ?? 0) + 1);
          });

          const entityNodes: GraphNode[] = entities.map((e: any) => ({
            id: e.entityId ?? e.id,
            label: e.canonicalLabel ?? e.label ?? e.key ?? 'unknown',
            type: mapNodeType(e.entityType),
            summary: e.summary ?? undefined,
            edgeCount: e.supportCount ?? 0,
            properties: {},
            sourceDocumentIds: [],
          }));

          const documentNodes: GraphNode[] = documents.map((d: any) => {
            const docId = d.document_id ?? d.documentId ?? d.id;
            return {
              id: docId,
              label: d.title ?? d.fileName ?? d.external_key ?? 'untitled',
              type: 'document' as GraphNodeType,
              summary: d.document_state ?? undefined,
              edgeCount: docEdgeCounts.get(docId) ?? 0,
              properties: {},
              sourceDocumentIds: [],
            };
          });

          const fallbackNodes: GraphNode[] = [...entityNodes, ...documentNodes];

          const relationEdges = relations.map((r: any) => ({
            id: r.relationId ?? r.id,
            sourceId: r.subjectEntityId,
            targetId: r.objectEntityId,
            label: r.predicate ?? '',
            weight: r.supportCount ?? 1,
          }));

          const documentEdges = documentLinks.map((link: any) => ({
            id: `dl-${link.documentId}-${link.targetNodeId}`,
            sourceId: link.documentId,
            targetId: link.targetNodeId,
            label: 'supports',
            weight: link.supportCount ?? 1,
          }));

          const fallbackEdges = [...relationEdges, ...documentEdges];

          // Store edges on the module level so GraphCanvas can use them
          edgesRef.current = fallbackEdges;

          const fallbackMeta: GraphMetadata = {
            nodeCount: fallbackNodes.length,
            edgeCount: fallbackEdges.length,
            hiddenDisconnectedCount: 0,
            status: fallbackNodes.length > 0 ? 'ready' : 'empty',
            convergenceStatus: 'current',
            recommendedLayout: undefined,
          };

          setAllNodes(fallbackNodes);
          setGraphMeta(fallbackMeta);
          setGraphStatus(fallbackMeta.status);
          return;
        }

        edgesRef.current = [];
        setAllNodes(nodes);
        setGraphMeta(meta);
        setGraphStatus(meta.status);
        if (meta.recommendedLayout) {
          setLayout(meta.recommendedLayout as LayoutType);
        }
      })
      .catch(err => {
        if (cancelled) return;
        setLoadError(err?.message ?? 'Failed to load graph');
        setGraphStatus('failed');
      });

    return () => { cancelled = true; };
  }, [activeLibrary]);

  // Fetch node detail when selected — different API for entities vs documents
  useEffect(() => {
    if (!activeLibrary || !selectedNode) {
      setSelectedDetail(null);
      return;
    }
    const basic = allNodes.find(n => n.id === selectedNode) ?? null;
    setSelectedDetail(basic);
    setDetailLoading(true);

    let cancelled = false;

    if (basic?.type === 'document') {
      // For documents, fetch document detail from content API
      import('@/api').then(({ documentsApi }) => {
        documentsApi.get(selectedNode)
          .then(doc => {
            if (cancelled) return;
            const enriched: GraphNode = {
              id: selectedNode,
              label: doc.fileName ?? basic.label,
              type: 'document',
              summary: doc.readinessSummary?.readinessKind ?? basic.summary,
              edgeCount: basic.edgeCount,
              properties: {},
              sourceDocumentIds: [],
            };
            const rev = doc.activeRevision ?? doc.active_revision;
            if (rev?.mime_type) enriched.properties['format'] = rev.mime_type;
            if (rev?.byte_size) enriched.properties['size'] = `${(rev.byte_size / 1024).toFixed(1)} KB`;
            if (rev?.revision_number) enriched.properties['revision'] = String(rev.revision_number);
            enriched.properties['state'] = doc.readinessSummary?.readinessKind ?? 'unknown';
            enriched.properties['activity'] = doc.readinessSummary?.activityStatus ?? 'unknown';
            if (doc.readinessSummary?.graphCoverageKind) enriched.properties['graph coverage'] = doc.readinessSummary.graphCoverageKind;
            setSelectedDetail(enriched);
          })
          .catch(() => { if (!cancelled) setSelectedDetail(basic); })
          .finally(() => { if (!cancelled) setDetailLoading(false); });
      });
      return () => { cancelled = true; };
    }

    // For entities/topics, use the knowledge entity API
    knowledgeApi.getEntity(activeLibrary.id, selectedNode)
      .then(detail => {
        if (cancelled) return;
        const entity = detail.entity ?? detail;
        const enriched: GraphNode = {
          id: entity.entityId ?? entity.id ?? selectedNode,
          label: entity.canonicalLabel ?? entity.label ?? basic?.label ?? '',
          type: mapNodeType(entity.entityType ?? entity.nodeType),
          summary: entity.summary ?? basic?.summary,
          edgeCount: entity.supportCount ?? basic?.edgeCount ?? 0,
          properties: {},
          sourceDocumentIds: [],
        };
        if (entity.entityType) enriched.properties['type'] = entity.entityType;
        if (entity.confidence != null) enriched.properties['confidence'] = String(Math.round(entity.confidence * 100)) + '%';
        if (entity.supportCount != null) enriched.properties['support count'] = String(entity.supportCount);
        if (entity.entityState) enriched.properties['state'] = entity.entityState;
        if (entity.aliases?.length) enriched.properties['aliases'] = entity.aliases.join(', ');
        if (detail.selectedNode?.relatedNodes) {
          enriched.sourceDocumentIds = (detail.selectedNode.supportingDocuments ?? []).map((d: any) => d.documentId);
        }
        setSelectedDetail(enriched);
      })
      .catch((err) => {
        console.error("Entity detail failed:", err);
        toast.error(err?.message || "Failed to load entity details");
      })
      .finally(() => {
        if (!cancelled) setDetailLoading(false);
      });

    return () => { cancelled = true; };
  }, [activeLibrary, selectedNode, allNodes]);

  const filteredNodes = useMemo(() => allNodes.filter(n => {
    if (nodeTypeFilter !== 'all' && n.type !== nodeTypeFilter) return false;
    if (searchQuery && !n.label.toLowerCase().includes(searchQuery.toLowerCase())) return false;
    return true;
  }), [allNodes, nodeTypeFilter, searchQuery]);

  const selected = selectedDetail ?? allNodes.find(n => n.id === selectedNode) ?? null;

  if (!activeLibrary) {
    return (
      <div className="flex-1 flex flex-col">
        <div className="page-header"><h1 className="text-lg font-bold tracking-tight">{t('graph.title')}</h1></div>
        <div className="empty-state flex-1">
          <div className="w-14 h-14 rounded-2xl bg-muted flex items-center justify-center mb-4">
            <Share2 className="h-7 w-7 text-muted-foreground" />
          </div>
          <h2 className="text-base font-bold tracking-tight">{t('graph.noLibrary')}</h2>
          <p className="text-sm text-muted-foreground mt-2">{t('graph.noLibraryDesc')}</p>
        </div>
      </div>
    );
  }

  return (
    <div className="flex-1 min-h-0 flex flex-col overflow-hidden">
      {/* Toolbar */}
      <div className="px-4 py-2.5 border-b flex items-center gap-2 flex-wrap" style={{
        background: 'linear-gradient(180deg, hsl(var(--card)), hsl(var(--background)))',
      }}>
        <div className="relative min-w-[180px]">
          <Search className="absolute left-2.5 top-1/2 -translate-y-1/2 h-3.5 w-3.5 text-muted-foreground" />
          <Input className="h-8 pl-8 text-xs" placeholder={t('graph.searchPlaceholder')} value={searchQuery} onChange={e => setSearchQuery(e.target.value)} />
        </div>

        <Select value={nodeTypeFilter} onValueChange={v => setNodeTypeFilter(v as typeof nodeTypeFilter)}>
          <SelectTrigger className="h-8 w-28 text-xs"><SelectValue /></SelectTrigger>
          <SelectContent>
            <SelectItem value="all">{t('graph.allTypes')}</SelectItem>
            <SelectItem value="document">{t('graph.documents')}</SelectItem>
            <SelectItem value="entity">{t('graph.entities')}</SelectItem>
            <SelectItem value="topic">{t('graph.topics')}</SelectItem>
          </SelectContent>
        </Select>

        <Select value={layout} onValueChange={v => setLayout(v as LayoutType)}>
          <SelectTrigger className="h-8 w-28 text-xs"><SelectValue /></SelectTrigger>
          <SelectContent>
            {LAYOUTS.map(l => <SelectItem key={l} value={l} className="capitalize">{l}</SelectItem>)}
          </SelectContent>
        </Select>

        <div className="flex items-center gap-0.5 bg-muted rounded-xl p-0.5 border border-border/50">
          <Tooltip><TooltipTrigger asChild><Button variant="ghost" size="icon" className="h-7 w-7 rounded-[9px]" onClick={() => setZoom(z => Math.min(20, z * 1.3))}><ZoomIn className="h-3.5 w-3.5" /></Button></TooltipTrigger><TooltipContent side="bottom" className="text-xs">{t('graph.zoomIn')}</TooltipContent></Tooltip>
          <Tooltip><TooltipTrigger asChild><Button variant="ghost" size="icon" className="h-7 w-7 rounded-[9px]" onClick={() => setZoom(z => Math.max(0.1, z / 1.3))}><ZoomOut className="h-3.5 w-3.5" /></Button></TooltipTrigger><TooltipContent side="bottom" className="text-xs">{t('graph.zoomOut')}</TooltipContent></Tooltip>
          <Tooltip><TooltipTrigger asChild><Button variant="ghost" size="icon" className="h-7 w-7 rounded-[9px]" onClick={() => { setZoom(1); setFitKey(k => k + 1); }}><Maximize2 className="h-3.5 w-3.5" /></Button></TooltipTrigger><TooltipContent side="bottom" className="text-xs">{t('graph.fit')}</TooltipContent></Tooltip>
        </div>

        <button className="h-7 px-2.5 text-xs flex items-center gap-1.5 rounded-lg hover:bg-muted transition-all duration-200 font-semibold" onClick={() => setShowFiltered(!showFiltered)}>
          {showFiltered ? <Eye className="h-3.5 w-3.5" /> : <EyeOff className="h-3.5 w-3.5" />} {t('graph.artifacts')}
        </button>

        {selectedNode && (
          <button className="h-7 px-2.5 text-xs flex items-center gap-1.5 rounded-lg hover:bg-muted transition-all duration-200 font-semibold" onClick={() => setSelectedNode(null)}>
            <X className="h-3.5 w-3.5" /> {t('graph.clear')}
          </button>
        )}

        <div className="ml-auto flex items-center gap-3 text-xs text-muted-foreground">
          <span className="tabular-nums font-semibold">{graphMeta?.nodeCount ?? 0} {t('graph.nodes')}</span>
          <span className="tabular-nums font-semibold">{graphMeta?.edgeCount ?? 0} {t('graph.edges')}</span>
          {(graphMeta?.hiddenDisconnectedCount ?? 0) > 0 && <span className="tabular-nums">{graphMeta!.hiddenDisconnectedCount} {t('graph.hidden')}</span>}
          <span className={`status-badge ${graphStatus === 'ready' ? 'status-ready' : graphStatus === 'partial' ? 'status-warning' : graphStatus === 'failed' ? 'status-failed' : 'status-processing'}`}>
            {graphStatus}
          </span>
        </div>
      </div>

      <div className="flex-1 min-h-0 relative overflow-hidden">
        <div className="absolute inset-0">
          {graphStatus === 'building' || graphStatus === 'rebuilding' ? (
            <div className="absolute inset-0 flex flex-col items-center justify-center bg-surface-sunken">
              <Loader2 className="h-8 w-8 animate-spin text-primary/60 mb-3" />
              <p className="text-sm font-semibold text-muted-foreground">{t('graph.loading')}</p>
            </div>
          ) : loadError ? (
            <div className="absolute inset-0 flex flex-col items-center justify-center bg-surface-sunken">
              <div className="w-14 h-14 rounded-2xl bg-muted flex items-center justify-center mb-4">
                <AlertTriangle className="h-7 w-7 text-status-failed" />
              </div>
              <h2 className="text-base font-bold tracking-tight">{t('graph.failedToLoad')}</h2>
              <p className="text-sm text-muted-foreground mt-2 max-w-sm text-center">{loadError}</p>
            </div>
          ) : filteredNodes.length === 0 ? (
            <div className="absolute inset-0 flex flex-col items-center justify-center bg-surface-sunken">
              <div className="w-14 h-14 rounded-2xl bg-muted flex items-center justify-center mb-4">
                <Share2 className="h-7 w-7 text-muted-foreground" />
              </div>
              <h2 className="text-base font-bold tracking-tight">
                {allNodes.length === 0 ? t('graph.noGraph') : t('graph.noMatchingNodes')}
              </h2>
              <p className="text-sm text-muted-foreground mt-2 max-w-sm text-center">
                {allNodes.length === 0 ? t('graph.noGraphDesc') : t('graph.noMatchingNodesDesc')}
              </p>
              {allNodes.length === 0 && (
                <Button variant="outline" size="sm" className="mt-4" onClick={() => navigate('/documents')}>
                  <FileText className="h-3.5 w-3.5 mr-1.5" /> {t('graph.goToDocuments')}
                </Button>
              )}
            </div>
          ) : (
            <GraphCanvas nodes={filteredNodes} edges={edgesRef.current} selectedId={selectedNode} onSelect={setSelectedNode} layout={layout} zoom={zoom} onZoomChange={setZoom} fitKey={fitKey} />
          )}

          {/* Legend */}
          <div className="absolute bottom-3 left-3 flex items-center gap-3 text-xs glass-panel rounded-xl px-4 py-2.5 shadow-lifted">
            <span className="flex items-center gap-1.5"><span className="w-2.5 h-2.5 rounded-full" style={{ background: NODE_COLORS.document }} /> {t('graph.document')}</span>
            <span className="flex items-center gap-1.5"><span className="w-2.5 h-2.5 rounded-full" style={{ background: NODE_COLORS.entity }} /> {t('graph.entity')}</span>
            <span className="flex items-center gap-1.5"><span className="w-2.5 h-2.5 rounded-full" style={{ background: NODE_COLORS.topic }} /> {t('graph.topic')}</span>
          </div>

          {graphMeta?.recommendedLayout && layout !== graphMeta.recommendedLayout && (
            <div className="absolute top-3 left-3 text-xs glass-panel rounded-xl px-4 py-2.5 shadow-lifted flex items-center gap-1.5">
              <AlertTriangle className="h-3 w-3 text-status-warning" />
              {t('graph.recommended')} <button className="font-bold text-primary hover:underline" onClick={() => setLayout(graphMeta.recommendedLayout as LayoutType)}>{graphMeta.recommendedLayout}</button>
            </div>
          )}
        </div>

        {selected && (() => {
          // Build ACTUALLY connected nodes using adjacency map
          const connectedIds = edgesRef.current
            .filter(e => e.sourceId === selected.id || e.targetId === selected.id)
            .map(e => e.sourceId === selected.id ? e.targetId : e.sourceId);
          const connectedNodes = connectedIds
            .map(id => allNodes.find(n => n.id === id))
            .filter((n): n is GraphNode => n != null)
            .slice(0, 20);
          const connectedDocs = connectedNodes.filter(n => n.type === 'document');
          const connectedEntities = connectedNodes.filter(n => n.type === 'entity');
          const connectedTopics = connectedNodes.filter(n => n.type === 'topic');

          return (
            <div className="absolute top-0 right-0 h-full w-80 lg:w-96 bg-card border-l shadow-xl z-20 overflow-y-auto animate-slide-in-right">
              <div className="p-4 border-b flex items-center justify-between">
                <h3 className="text-sm font-bold truncate tracking-tight">{selected.label}</h3>
                <div className="flex items-center gap-1">
                  {detailLoading && <Loader2 className="h-3.5 w-3.5 animate-spin text-muted-foreground" />}
                  <button onClick={() => setSelectedNode(null)} className="p-1.5 rounded-lg hover:bg-muted transition-colors" aria-label="Close"><X className="h-4 w-4" /></button>
                </div>
              </div>
              <div className="p-4 space-y-4">
                {/* Type & connections header */}
                <div className="flex items-center gap-2.5">
                  <span className="w-3 h-3 rounded-full" style={{ background: NODE_COLORS[selected.type] }} />
                  <span className="text-sm font-semibold capitalize">{selected.type}</span>
                  <span className="text-xs text-muted-foreground ml-auto tabular-nums font-medium">{connectedIds.length} {t('graph.connections')}</span>
                </div>

                {/* Summary */}
                {selected.summary && (
                  <div><div className="section-label mb-1">{t('graph.summary')}</div><p className="text-sm leading-relaxed text-muted-foreground">{selected.summary}</p></div>
                )}

                {/* Properties */}
                {Object.keys(selected.properties).length > 0 && (
                  <div>
                    <div className="section-label mb-1.5">{t('graph.properties')}</div>
                    <div className="space-y-1">
                      {Object.entries(selected.properties).map(([k, v]) => (
                        <div key={k} className="flex justify-between text-xs">
                          <span className="text-muted-foreground capitalize">{k}</span>
                          <span className="font-semibold text-foreground">{v}</span>
                        </div>
                      ))}
                    </div>
                  </div>
                )}

                {/* Actions */}
                <div className="flex gap-2">
                  {selected.type === 'document' && (
                    <Button variant="outline" size="sm" className="text-xs h-7" onClick={() => navigate(`/documents?highlight=${selected.id}`)}>
                      <FileText className="h-3 w-3 mr-1" /> {t('graph.viewDocument')}
                    </Button>
                  )}
                  <Button variant="outline" size="sm" className="text-xs h-7" onClick={() => {
                    setNodeTypeFilter('all');
                    setSearchQuery(selected.label.split(' ')[0]);
                  }}>
                    <Search className="h-3 w-3 mr-1" /> {t('graph.findSimilar')}
                  </Button>
                </div>

                {/* Connected Documents */}
                {connectedDocs.length > 0 && (
                  <div>
                    <div className="section-label mb-1.5">{t('graph.sourceDocuments')} ({connectedDocs.length})</div>
                    <div className="space-y-0.5">
                      {connectedDocs.map(n => (
                        <button key={n.id} className="w-full flex items-center gap-2 p-2 rounded-lg hover:bg-accent/50 text-left text-xs transition-colors" onClick={() => setSelectedNode(n.id)}>
                          <span className="w-2 h-2 rounded-full shrink-0" style={{ background: NODE_COLORS.document }} />
                          <span className="truncate font-medium">{n.label}</span>
                        </button>
                      ))}
                    </div>
                  </div>
                )}

                {/* Connected Entities */}
                {connectedEntities.length > 0 && (
                  <div>
                    <div className="section-label mb-1.5">{t('graph.connectedEntities')} ({connectedEntities.length})</div>
                    <div className="space-y-0.5">
                      {connectedEntities.slice(0, 15).map(n => (
                        <button key={n.id} className="w-full flex items-center gap-2 p-2 rounded-lg hover:bg-accent/50 text-left text-xs transition-colors" onClick={() => setSelectedNode(n.id)}>
                          <span className="w-2 h-2 rounded-full shrink-0" style={{ background: NODE_COLORS.entity }} />
                          <span className="truncate">{n.label}</span>
                          {n.edgeCount > 0 && <span className="text-[10px] text-muted-foreground ml-auto tabular-nums">{n.edgeCount}</span>}
                        </button>
                      ))}
                      {connectedEntities.length > 15 && <span className="text-xs text-muted-foreground pl-6">+{connectedEntities.length - 15} more</span>}
                    </div>
                  </div>
                )}

                {/* Connected Topics */}
                {connectedTopics.length > 0 && (
                  <div>
                    <div className="section-label mb-1.5">{t('graph.connectedTopics')} ({connectedTopics.length})</div>
                    <div className="space-y-0.5">
                      {connectedTopics.slice(0, 10).map(n => (
                        <button key={n.id} className="w-full flex items-center gap-2 p-2 rounded-lg hover:bg-accent/50 text-left text-xs transition-colors" onClick={() => setSelectedNode(n.id)}>
                          <span className="w-2 h-2 rounded-full shrink-0" style={{ background: NODE_COLORS.topic }} />
                          <span className="truncate">{n.label}</span>
                        </button>
                      ))}
                    </div>
                  </div>
                )}

                {connectedNodes.length === 0 && !detailLoading && (
                  <p className="text-xs text-muted-foreground">{t('graph.noConnections')}</p>
                )}
              </div>
            </div>
          );
        })()}
      </div>
    </div>
  );
}
