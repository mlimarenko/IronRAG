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

const LAYOUTS = ['cloud', 'circle', 'rings', 'lanes', 'clusters', 'islands', 'spiral'] as const;
type LayoutType = typeof LAYOUTS[number];

const NODE_COLORS: Record<string, string> = {
  document: '#3b82f6',      // blue
  person: '#ec4899',        // pink
  organization: '#64748b',  // slate
  location: '#84cc16',      // lime
  event: '#f43f5e',         // rose
  artifact: '#06b6d4',      // cyan
  natural: '#22c55e',       // green
  process: '#a855f7',       // purple
  concept: '#f59e0b',       // amber
  attribute: '#0ea5e9',     // sky
  entity: '#78716c',        // stone
};

function scaledRadii(total: number): Record<GraphNodeType, { min: number; max: number }> {
  // Scale node sizes down as count grows — prevent overlapping
  const s = total > 100 ? 0.4 : total > 50 ? 0.6 : total > 20 ? 0.8 : 1.0;
  return {
    document: { min: 1.5 * s, max: 2.8 * s },
    person: { min: 1.0 * s, max: 1.8 * s },
    organization: { min: 1.0 * s, max: 1.8 * s },
    location: { min: 1.0 * s, max: 1.8 * s },
    event: { min: 1.0 * s, max: 1.8 * s },
    artifact: { min: 1.0 * s, max: 1.8 * s },
    natural: { min: 1.0 * s, max: 1.8 * s },
    process: { min: 1.0 * s, max: 1.8 * s },
    concept: { min: 0.7 * s, max: 1.3 * s },
    attribute: { min: 1.0 * s, max: 1.8 * s },
    entity: { min: 1.0 * s, max: 1.8 * s },
  };
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
  if (t === 'document') return 'document';
  if (t === 'person') return 'person';
  if (t === 'organization') return 'organization';
  if (t === 'location') return 'location';
  if (t === 'event') return 'event';
  if (t === 'artifact') return 'artifact';
  if (t === 'natural') return 'natural';
  if (t === 'process') return 'process';
  if (t === 'concept') return 'concept';
  if (t === 'attribute') return 'attribute';
  if (t === 'entity') return 'entity';
  // Backward compat for legacy type names
  if (t === 'topic') return 'concept';
  if (t === 'technology') return 'artifact';
  if (t === 'api') return 'artifact';
  if (t === 'code_symbol') return 'artifact';
  if (t === 'natural_kind') return 'natural';
  if (t === 'metric') return 'attribute';
  if (t === 'regulation') return 'artifact';
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


/**
 * High-performance Canvas2D graph renderer.
 * ALL interaction state (pan, zoom, hover) lives in refs — zero React re-renders during drag/zoom.
 * React only re-renders when nodes/edges/selectedId/layout change (data changes).
 */
function GraphCanvas({ nodes, edges, selectedId, onSelect, layout, zoom, onZoomChange, fitKey }: {
  nodes: GraphNode[]; edges: EdgeData[]; selectedId: string | null; onSelect: (id: string | null) => void; layout: LayoutType; zoom: number; onZoomChange: (z: number) => void; fitKey: number;
}) {
  const containerRef = useRef<HTMLDivElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const rafRef = useRef<number>(0);
  const dragStartRef = useRef({ x: 0, y: 0 });
  const panStartRef = useRef({ x: 0, y: 0 });
  const draggedRef = useRef(false);
  const panRef = useRef({ x: 0, y: 0 });
  const isPanningRef = useRef(false);
  const hoveredNodeRef = useRef<string | null>(null);
  const hoverTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const nodePositionsRef = useRef<Map<string, { x: number; y: number }>>(new Map());
  const dragNodeRef = useRef<string | null>(null);
  const zoomRef = useRef(zoom);
  const selectedIdRef = useRef(selectedId);
  const drawScheduled = useRef(false);

  // Keep refs in sync with props
  useEffect(() => { zoomRef.current = zoom; }, [zoom]);
  useEffect(() => { selectedIdRef.current = selectedId; scheduleRedraw(); }, [selectedId]);

  // Schedule a single redraw on next animation frame (coalesces multiple calls)
  const scheduleRedraw = useCallback(() => {
    if (drawScheduled.current) return;
    drawScheduled.current = true;
    rafRef.current = requestAnimationFrame(() => {
      drawScheduled.current = false;
      drawCanvas();
    });
  }, []);

  // Placeholder — will be assigned after drawCanvas is defined
  const drawCanvasRef = useRef<() => void>(() => {});
  const drawCanvas = useCallback(() => drawCanvasRef.current(), []);

  // Compute viewBox-equivalent size
  const viewSize = useMemo(() => Math.max(100, Math.ceil(Math.sqrt(nodes.length) * 14)), [nodes.length]);

  // Pre-compute node index map for O(1) position lookups
  const nodeIndexMap = useMemo(() => {
    const m = new Map<string, number>();
    nodes.forEach((n, i) => m.set(n.id, i));
    return m;
  }, [nodes]);

  // Pre-compute adjacency map: nodeId -> Set of connected nodeIds
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
    return { maxChars, fontSize, edgeThreshold, topNodeIds };
  }, [nodes]);

  // Pre-compute cluster/island metadata
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

  useEffect(() => {
    nodePositionsRef.current = new Map(); scheduleRedraw();
  }, [nodes, edges, layout, viewSize]);

  useEffect(() => {
    panRef.current = { x: 0, y: 0 }; scheduleRedraw();
  }, [fitKey]);

  // Wheel zoom — ref-only, no React re-render per wheel tick
  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    const handler = (e: WheelEvent) => {
      e.preventDefault();
      const delta = e.deltaY > 0 ? -0.1 : 0.1;
      zoomRef.current = Math.min(20, Math.max(0.1, zoomRef.current + delta));
      scheduleRedraw();
    };
    el.addEventListener('wheel', handler, { passive: false });
    return () => el.removeEventListener('wheel', handler);
  }, [zoom, onZoomChange]);

  // Layout position calculator (same logic as before, no nodePositions dependency — uses raw layout)
  const getLayoutPosition = useCallback((index: number, total: number) => {
    const sz = Math.max(100, Math.ceil(Math.sqrt(total) * 14));
    const margin = sz * 0.06;
    const usable = sz - margin * 2;
    const cx = sz / 2, cy = sz / 2;

    switch (layout) {
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
  // clusterMeta and nodes are stable references per render
  }, [layout, clusterMeta, nodes]);

  // Reset camera when layout changes
  useEffect(() => {
    panRef.current = { x: 0, y: 0 }; scheduleRedraw();
  }, [layout]);

  // Seed nodePositions so dragging works
  useEffect(() => {
    if (nodes.length === 0) return;
    const initial = new Map<string, { x: number; y: number }>();
    nodes.forEach((_node, i) => {
      const pos = getLayoutPosition(i, nodes.length);
      initial.set(nodes[i].id, pos);
    });
    nodePositionsRef.current = initial; scheduleRedraw();
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [layout, nodes]);

  // ---------------------------------------------------------------------------
  // Canvas2D draw loop
  // ---------------------------------------------------------------------------
  useEffect(() => {
    const canvas = canvasRef.current;
    const container = containerRef.current;
    if (!canvas || !container) return;
    const ctx = canvas.getContext('2d');
    if (!ctx) return;

    let cancelled = false;

    const draw = () => {
      if (cancelled) return;
      const rect = container.getBoundingClientRect();
      const dpr = window.devicePixelRatio || 1;
      const w = rect.width;
      const h = rect.height;

      // Resize canvas if needed
      if (canvas.width !== Math.round(w * dpr) || canvas.height !== Math.round(h * dpr)) {
        canvas.width = Math.round(w * dpr);
        canvas.height = Math.round(h * dpr);
        canvas.style.width = `${w}px`;
        canvas.style.height = `${h}px`;
      }

      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      ctx.clearRect(0, 0, w, h);

      // Background — transparent to inherit page theme
      ctx.clearRect(0, 0, w, h);

      // Apply transform: center + pan + zoomRef.current, map viewBox coords to screen
      ctx.save();
      const scale = (Math.min(w, h) / viewSize) * zoomRef.current;
      const offsetX = w / 2 + panRef.current.x - (viewSize / 2) * scale;
      const offsetY = h / 2 + panRef.current.y - (viewSize / 2) * scale;

      // Compute viewport bounds in viewBox coordinates for culling
      const vpLeft = -offsetX / scale;
      const vpTop = -offsetY / scale;
      const vpRight = (w - offsetX) / scale;
      const vpBottom = (h - offsetY) / scale;
      const vpMargin = 10; // extra margin in viewBox units

      // Helper to check if a point is in viewport
      const inViewport = (x: number, y: number) =>
        x >= vpLeft - vpMargin && x <= vpRight + vpMargin &&
        y >= vpTop - vpMargin && y <= vpBottom + vpMargin;

      // Transform to viewBox coordinate system
      ctx.translate(offsetX, offsetY);
      ctx.scale(scale, scale);

      const positions = nodePositionsRef.current;
      const total = nodes.length;

      // Build position cache for this frame
      const posCache = new Map<string, { x: number; y: number }>();
      for (let i = 0; i < total; i++) {
        const node = nodes[i];
        const pos = positions.has(node.id) ? positions.get(node.id)! : getLayoutPosition(i, total);
        posCache.set(node.id, pos);
      }

      // --- Draw edges ---
      const baseEdgeWidth = 0.15 / Math.max(zoomRef.current, 0.3);
      const isLargeGraph = total > 1000;
      const showBgEdges = !isLargeGraph || zoomRef.current > 0.5;
      // Curvature offset for quadratic bezier (perpendicular distance)
      // Curvature scales with edge length for visible curves
      const curvatureFactor = 0.15; // 15% of edge length as perpendicular offset

      if (showBgEdges) {
        ctx.strokeStyle = selectedIdRef.current ? 'rgba(148, 163, 184, 0.08)' : 'rgba(148, 163, 184, 0.3)';
        ctx.lineWidth = selectedIdRef.current ? baseEdgeWidth * 0.4 : baseEdgeWidth * 0.9;
        let edgesDrawn = 0;
        const edgeBudget = isLargeGraph ? Math.round(2000 / Math.max(zoomRef.current, 0.5)) : edges.length;
        ctx.beginPath();
        for (const edge of edges) {
          if (edgesDrawn >= edgeBudget) break;
          const sp = posCache.get(edge.sourceId);
          const tp = posCache.get(edge.targetId);
          if (!sp || !tp) continue;
          if (!inViewport(sp.x, sp.y) && !inViewport(tp.x, tp.y)) continue;
          if (selectedIdRef.current && (selectedIdRef.current === edge.sourceId || selectedIdRef.current === edge.targetId)) continue;
          // Quadratic bezier curve — control point offset perpendicular to the line
          const mx = (sp.x + tp.x) / 2;
          const my = (sp.y + tp.y) / 2;
          const dx = tp.x - sp.x;
          const dy = tp.y - sp.y;
          const len = Math.sqrt(dx * dx + dy * dy);
          const nx = len > 0 ? -dy / len : 0;
          const ny = len > 0 ? dx / len : 0;
          const curve = len * curvatureFactor;
          const cpx = mx + nx * curve;
          const cpy = my + ny * curve;
          ctx.moveTo(sp.x, sp.y);
          ctx.quadraticCurveTo(cpx, cpy, tp.x, tp.y);
          edgesDrawn++;
        }
        ctx.stroke();
      }

      // Connected edges — always visible, curved, highlighted
      if (selectedIdRef.current) {
        ctx.beginPath();
        ctx.strokeStyle = 'rgba(59, 130, 246, 0.9)';
        ctx.lineWidth = baseEdgeWidth * 3;
        const connectedEdges: EdgeData[] = [];
        for (const edge of edges) {
          if (selectedIdRef.current === edge.sourceId || selectedIdRef.current === edge.targetId) {
            const sp = posCache.get(edge.sourceId);
            const tp = posCache.get(edge.targetId);
            if (!sp || !tp) continue;
            const mx = (sp.x + tp.x) / 2;
            const my = (sp.y + tp.y) / 2;
            const dx = tp.x - sp.x;
            const dy = tp.y - sp.y;
            const len = Math.sqrt(dx * dx + dy * dy);
            const nx = len > 0 ? -dy / len : 0;
            const ny = len > 0 ? dx / len : 0;
            const curve = len * curvatureFactor;
            const cpx = mx + nx * curve;
            const cpy = my + ny * curve;
            ctx.moveTo(sp.x, sp.y);
            ctx.quadraticCurveTo(cpx, cpy, tp.x, tp.y);
            connectedEdges.push(edge);
          }
        }
        ctx.stroke();

        // Edge labels for connected edges at sufficient zoomRef.current
        if (zoomRef.current > 0.6) {
          ctx.fillStyle = '#9ca3af';
          const eFontSize = labelConfig.fontSize * 0.65;
          ctx.font = `${eFontSize}px sans-serif`;
          ctx.textAlign = 'center';
          ctx.textBaseline = 'middle';
          for (const edge of connectedEdges) {
            if (!edge.label) continue;
            const sp = posCache.get(edge.sourceId);
            const tp = posCache.get(edge.targetId);
            if (!sp || !tp) continue;
            // Bezier midpoint at t=0.5: (P0 + 2*CP + P2) / 4
            const dx = tp.x - sp.x;
            const dy = tp.y - sp.y;
            const len = Math.sqrt(dx * dx + dy * dy);
            const nx2 = len > 0 ? -dy / len : 0;
            const ny2 = len > 0 ? dx / len : 0;
            const curve2 = len * curvatureFactor;
            const cpx2 = (sp.x + tp.x) / 2 + nx2 * curve2;
            const cpy2 = (sp.y + tp.y) / 2 + ny2 * curve2;
            const lx = (sp.x + 2 * cpx2 + tp.x) / 4;
            const ly = (sp.y + 2 * cpy2 + tp.y) / 4;
            ctx.save();
            ctx.translate(lx, ly);
            let angle = Math.atan2(tp.y - sp.y, tp.x - sp.x);
            if (angle > Math.PI / 2) angle -= Math.PI;
            if (angle < -Math.PI / 2) angle += Math.PI;
            ctx.rotate(angle);
            ctx.globalAlpha = 0.7;
            ctx.fillText(edge.label, 0, -eFontSize * 0.7);
            ctx.restore();
          }
          ctx.globalAlpha = 1;
        }
      }

      // --- Draw nodes ---
      const selectedAdj = selectedIdRef.current ? adjacency.get(selectedIdRef.current) : undefined;
      const hovered = hoveredNodeRef.current;

      for (let i = 0; i < total; i++) {
        const node = nodes[i];
        const pos = posCache.get(node.id)!;
        if (!inViewport(pos.x, pos.y)) continue;

        const r = nodeRadius(node);
        const color = NODE_COLORS[node.type] || NODE_COLORS.entity;
        const isSelected = node.id === selectedIdRef.current;
        const isConnectedToSelected = selectedAdj?.has(node.id) ?? false;
        const dimmed = !!selectedIdRef.current && !isSelected && !isConnectedToSelected;
        const isHovered = node.id === hovered;

        // Glow for selected node
        if (isSelected) {
          const grad = ctx.createRadialGradient(pos.x, pos.y, 0, pos.x, pos.y, r * 2.5);
          grad.addColorStop(0, color + '4d'); // 30% opacity
          grad.addColorStop(1, color + '00');
          ctx.beginPath();
          ctx.arc(pos.x, pos.y, r * 2.5, 0, Math.PI * 2);
          ctx.fillStyle = grad;
          ctx.fill();
        }

        // Node circle
        ctx.beginPath();
        ctx.arc(pos.x, pos.y, r, 0, Math.PI * 2);
        ctx.globalAlpha = dimmed ? 0.15 : 0.9;
        ctx.fillStyle = color;
        ctx.fill();
        if (isSelected) {
          ctx.strokeStyle = '#e2e8f0';
          ctx.lineWidth = r * 0.12;
          ctx.stroke();
        }
        ctx.globalAlpha = 1;
      }

      // --- Draw labels (level-of-detail) ---
      const showLabels = zoomRef.current > 0.4 || total < 200;
      if (showLabels) {
        ctx.textAlign = 'center';
        ctx.textBaseline = 'top';
        for (let i = 0; i < total; i++) {
          const node = nodes[i];
          const pos = posCache.get(node.id)!;
          if (!inViewport(pos.x, pos.y)) continue;

          const isSelected = node.id === selectedIdRef.current;
          const isConnectedToSelected = selectedAdj?.has(node.id) ?? false;
          const isHovered = node.id === hovered;
          const dimmed = !!selectedIdRef.current && !isSelected && !isConnectedToSelected;

          // Determine if label should show
          const showLabel = isSelected || isConnectedToSelected || isHovered || labelConfig.topNodeIds.has(node.id);
          if (!showLabel) continue;
          // At medium zoomRef.current, skip low-support nodes
          if (zoomRef.current < 0.8 && !isSelected && !isConnectedToSelected && !isHovered && node.edgeCount < labelConfig.edgeThreshold) continue;

          const r = nodeRadius(node);
          const text = node.label.length > labelConfig.maxChars ? node.label.slice(0, labelConfig.maxChars - 1) + '\u2026' : node.label;
          const weight = isSelected ? '700' : (isConnectedToSelected || isHovered) ? '600' : '500';
          ctx.font = `${weight} ${labelConfig.fontSize}px sans-serif`;
          ctx.globalAlpha = dimmed ? 0.15 : isSelected || isHovered ? 1 : 0.75;
          ctx.fillStyle = '#94a3b8';
          ctx.fillText(text, pos.x, pos.y + r + labelConfig.fontSize * 0.3);
        }
        ctx.globalAlpha = 1;
      }

      ctx.restore();
    };

    // Store draw function for imperative redraws via scheduleRedraw
    drawCanvasRef.current = draw;

    // Initial draw
    rafRef.current = requestAnimationFrame(draw);
    return () => {
      cancelled = true;
      cancelAnimationFrame(rafRef.current);
    };
  // Only re-create draw function when data/layout changes (not on pan/zoom/hover)
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [nodes, edges, layout, viewSize, adjacency, labelConfig, nodeRadius, getLayoutPosition]);

  // ---------------------------------------------------------------------------
  // Hit testing: convert client coords to viewBox coords
  // ---------------------------------------------------------------------------
  const clientToViewBox = useCallback((clientX: number, clientY: number): { x: number; y: number } => {
    const container = containerRef.current;
    if (!container) return { x: 0, y: 0 };
    const rect = container.getBoundingClientRect();
    const w = rect.width;
    const h = rect.height;
    const scale = (Math.min(w, h) / viewSize) * zoomRef.current;
    const offsetX = w / 2 + panRef.current.x - (viewSize / 2) * scale;
    const offsetY = h / 2 + panRef.current.y - (viewSize / 2) * scale;
    return {
      x: (clientX - rect.left - offsetX) / scale,
      y: (clientY - rect.top - offsetY) / scale,
    };
  }, [viewSize]);

  // Find nearest node to a viewBox coordinate
  const findNodeAt = useCallback((vx: number, vy: number): string | null => {
    let bestId: string | null = null;
    let bestDist = Infinity;
    const hitRadius = Math.max(3, 8 / zoomRef.current); // generous hit area
    for (let i = 0; i < nodes.length; i++) {
      const node = nodes[i];
      const pos = nodePositionsRef.current.has(node.id)
        ? nodePositionsRef.current.get(node.id)!
        : getLayoutPosition(i, nodes.length);
      const dx = pos.x - vx;
      const dy = pos.y - vy;
      const dist = Math.sqrt(dx * dx + dy * dy);
      const r = nodeRadius(node);
      if (dist < Math.max(r, hitRadius) && dist < bestDist) {
        bestDist = dist;
        bestId = node.id;
      }
    }
    return bestId;
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [nodes, nodeRadius, getLayoutPosition]);

  // Debounced hover
  const handlePointerMoveHover = useCallback((clientX: number, clientY: number) => {
    const vb = clientToViewBox(clientX, clientY);
    const nodeId = findNodeAt(vb.x, vb.y);
    if (nodeId === hoveredNodeRef.current) return;
    hoveredNodeRef.current = nodeId;
    if (hoverTimerRef.current) clearTimeout(hoverTimerRef.current);
    hoverTimerRef.current = setTimeout(() => {
      scheduleRedraw();
    }, 50);
  }, [clientToViewBox, findNodeAt]);

  // ---------------------------------------------------------------------------
  // Pointer event handlers
  // ---------------------------------------------------------------------------
  const handlePointerDown = (e: ReactPointerEvent<HTMLDivElement>) => {
    if (e.button !== 0) return;
    if (dragNodeRef.current) return;

    // Check if clicking on a node
    const vb = clientToViewBox(e.clientX, e.clientY);
    const hitNode = findNodeAt(vb.x, vb.y);
    if (hitNode) {
      // Start node drag
      dragNodeRef.current = hitNode;
      dragNodeRef.current = hitNode;
      draggedRef.current = false;
      return;
    }

    draggedRef.current = false;
    dragStartRef.current = { x: e.clientX, y: e.clientY };
    panStartRef.current = panRef.current;
    isPanningRef.current = true;
  };

  const handlePointerMove = (e: ReactPointerEvent<HTMLDivElement>) => {
    // Node dragging
    if (dragNodeRef.current) {
      const vb = clientToViewBox(e.clientX, e.clientY);
      nodePositionsRef.current.set(dragNodeRef.current!, { x: vb.x, y: vb.y });
      scheduleRedraw();
      draggedRef.current = true;
      return;
    }

    if (!isPanningRef.current) {
      // Hover detection
      handlePointerMoveHover(e.clientX, e.clientY);
      return;
    }

    const deltaX = e.clientX - dragStartRef.current.x;
    const deltaY = e.clientY - dragStartRef.current.y;
    if (Math.abs(deltaX) > 3 || Math.abs(deltaY) > 3) {
      draggedRef.current = true;
      isPanningRef.current = true;
    }
    if (draggedRef.current) {
      panRef.current = { x: panStartRef.current.x + deltaX, y: panStartRef.current.y + deltaY }; scheduleRedraw();
    }
  };

  const handlePointerUp = (_e: ReactPointerEvent<HTMLDivElement>) => {
    if (dragNodeRef.current) {
      dragNodeRef.current = null;
      dragNodeRef.current = null;
      return;
    }
    isPanningRef.current = false;
    isPanningRef.current = false;
  };

  const handleCanvasClick = (e: React.MouseEvent<HTMLDivElement>) => {
    if (draggedRef.current) {
      draggedRef.current = false;
      return;
    }
    // Check if clicking on a node
    const vb = clientToViewBox(e.clientX, e.clientY);
    const hitNode = findNodeAt(vb.x, vb.y);
    if (hitNode) {
      onSelect(hitNode);
    } else {
      onSelect(null);
    }
  };

  return (
    <div
      ref={containerRef}
      className="w-full h-full relative overflow-hidden cursor-grab"
      onClick={handleCanvasClick}
      onPointerDown={handlePointerDown}
      onPointerMove={handlePointerMove}
      onPointerUp={handlePointerUp}
      onPointerCancel={handlePointerUp}
      style={{ touchAction: 'none' }}
    >
      <canvas ref={canvasRef} className="absolute inset-0 w-full h-full" />
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
  const [activeTypes, setActiveTypes] = useState<Set<GraphNodeType>>(new Set());
  const [layout, setLayout] = useState<LayoutType>('cloud');
  const [zoom, setZoom] = useState(1);
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
    if (activeTypes.size > 0 && !activeTypes.has(n.type)) return false;
    if (searchQuery && !n.label.toLowerCase().includes(searchQuery.toLowerCase())) return false;
    return true;
  }), [allNodes, activeTypes, searchQuery]);

  const effectiveLayout = layout;

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

        <div className="relative">
          <Button variant="outline" size="sm" className="h-8 text-xs gap-1.5" onClick={() => {
            const el = document.getElementById('type-filter-popover');
            if (el) el.classList.toggle('hidden');
          }}>
            <Filter className="h-3 w-3" />
            {activeTypes.size === 0 ? t('graph.allTypes') : `${activeTypes.size} ${activeTypes.size === 1 ? 'type' : 'types'}`}
          </Button>
          <div id="type-filter-popover" className="hidden absolute top-full left-0 mt-1 z-50 bg-popover border rounded-lg shadow-lg p-2 space-y-0.5 min-w-[160px]">
            <button className="w-full text-left px-2 py-1 text-xs rounded hover:bg-muted" onClick={() => setActiveTypes(new Set())}>
              {t('graph.allTypes')}
            </button>
            <div className="border-t my-1" />
            {(Object.keys(NODE_COLORS) as GraphNodeType[]).map(type => (
              <label key={type} className="flex items-center gap-2 px-2 py-1 text-xs rounded hover:bg-muted cursor-pointer">
                <input
                  type="checkbox"
                  className="h-3.5 w-3.5 rounded"
                  checked={activeTypes.has(type)}
                  onChange={() => {
                    setActiveTypes(prev => {
                      const next = new Set(prev);
                      if (next.has(type)) next.delete(type);
                      else next.add(type);
                      return next;
                    });
                  }}
                />
                <span className="inline-block w-2.5 h-2.5 rounded-full" style={{ backgroundColor: NODE_COLORS[type] }} />
                {type === 'natural' ? 'Natural' : type === 'code_symbol' ? 'Code' : type.charAt(0).toUpperCase() + type.slice(1)}
              </label>
            ))}
          </div>
        </div>

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
            <GraphCanvas nodes={filteredNodes} edges={edgesRef.current} selectedId={selectedNode} onSelect={setSelectedNode} layout={effectiveLayout} zoom={zoom} onZoomChange={setZoom} fitKey={fitKey} />
          )}

          {/* Legend */}
          <div className="absolute bottom-3 left-3 flex items-center gap-3 text-xs glass-panel rounded-xl px-4 py-2.5 shadow-lifted">
            {Object.entries(NODE_COLORS).map(([type, color]) => (
              <span key={type} className="flex items-center gap-1.5">
                <span className="w-2.5 h-2.5 rounded-full" style={{ background: color }} />
                {type.charAt(0).toUpperCase() + type.slice(1)}
              </span>
            ))}
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
          const connectedConcepts = connectedNodes.filter(n => n.type === 'concept');

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
                    setSearchQuery(selected.label);
                  }}>
                    <Search className="h-3 w-3 mr-1" /> {t('graph.findSimilar')}
                  </Button>
                  {searchQuery && (
                    <Button variant="ghost" size="sm" className="text-xs h-7" onClick={() => {
                      setSearchQuery('');
                      setActiveTypes(new Set());
                    }}>
                      <X className="h-3 w-3 mr-1" /> {t('graph.resetFilter') || 'Reset'}
                    </Button>
                  )}
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

                {/* Connected Concepts */}
                {connectedConcepts.length > 0 && (
                  <div>
                    <div className="section-label mb-1.5">{t('graph.connectedConcepts')} ({connectedConcepts.length})</div>
                    <div className="space-y-0.5">
                      {connectedConcepts.slice(0, 10).map(n => (
                        <button key={n.id} className="w-full flex items-center gap-2 p-2 rounded-lg hover:bg-accent/50 text-left text-xs transition-colors" onClick={() => setSelectedNode(n.id)}>
                          <span className="w-2 h-2 rounded-full shrink-0" style={{ background: NODE_COLORS.concept }} />
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
