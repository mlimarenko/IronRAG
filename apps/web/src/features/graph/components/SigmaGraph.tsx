import { memo, useCallback, useContext, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import Graph from 'graphology';
import Sigma from 'sigma';
import type { CameraState } from 'sigma/types';
import { Loader2 } from 'lucide-react';
import { EdgeCurvedArrowProgram } from '@sigma/edge-curve';
import type { GraphNode } from '@/shared/types';
import {
  buildGraphCanvasLabel,
  buildGraphFocusLabel,
  GRAPH_EDGE_DENSE_RENDER_CAP,
  GRAPH_EDGE_COLORS,
  GRAPH_EDGE_RENDER_CAP,
  GRAPH_NODE_COLORS,
  selectProminentGraphLabelIds,
  type GraphLayoutType,
} from '@/features/graph/model/config';
import { applyGraphLayout } from '@/features/graph/model/layouts';
import { computeGraphLayoutOffThread } from '@/features/graph/workers/graphLayoutClient';
import { PreferencesContext } from '@/shared/contexts/preferences-context';

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
  /** Called once after Sigma is initialized with a stable `fitView`
   *  callback. The parent stores this and calls it from the toolbar. */
  onFitViewReady?: (fitView: () => void) => void;
  /** When false (default), draw the smooth overview edge sample. When true,
   *  draw a denser Sigma base sample while keeping a hard render cap. The
   *  full topology still drives adjacency, selection, inspectors, and the
   *  dense GPU overlay; Sigma itself only carries the interaction-friendly
   *  sample so Firefox does not reindex hundreds of thousands of edges. */
  showDenseEdges?: boolean;
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

type NeighborhoodOverlayMode = 'hover' | 'selected' | 'drag';

type NeighborhoodOverlayFocus = {
  nodeId: string;
  mode: NeighborhoodOverlayMode;
} | null;

type AllEdgesCoordinateLayerState = {
  kind: 'coordinate';
  gl: WebGLRenderingContext | WebGL2RenderingContext;
  program: WebGLProgram;
  buffer: WebGLBuffer;
  positionLocation: number;
  matrixLocation: WebGLUniformLocation;
  colorLocation: WebGLUniformLocation;
};

type AllEdgesIndexedLayerState = {
  kind: 'indexed';
  gl: WebGL2RenderingContext;
  program: WebGLProgram;
  edgeBuffer: WebGLBuffer;
  positionTexture: WebGLTexture;
  edgeDataLocation: number;
  matrixLocation: WebGLUniformLocation;
  colorLocation: WebGLUniformLocation;
  positionTextureLocation: WebGLUniformLocation;
  positionTextureWidthLocation: WebGLUniformLocation;
  nodeIndexById: Map<string, number>;
  positionTextureWidth: number;
  positionTextureHeight: number;
  positionTextureData: Float32Array;
  scratchTexel: Float32Array;
};

type AllEdgesLayerState = AllEdgesCoordinateLayerState | AllEdgesIndexedLayerState;

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
const SIGMA_NODE_CANVAS_LAYERS = ['nodes', 'labels', 'hovers', 'hoverNodes', 'mouse'] as const;
const waitForAnimationFrame = () =>
  new Promise<void>((resolve) => {
    requestAnimationFrame(() => resolve());
  });
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
/// that density) and turning them off shaves meaningful work from the
/// dense-graph per-frame budget.
const LABELS_DISABLED_NODE_THRESHOLD = 15000;
/// Above this node count, the initial layout is computed in a Web
/// Worker so it never blocks the main thread. Below it, the sync
/// codepath is cheaper: serializing the node/edge arrays, spinning up
/// a postMessage round-trip, and deserializing the float positions is
/// ~20 ms of overhead that is not recovered on tiny graphs. 3000 is
/// roughly where `applyGraphLayout` starts to exceed a 16 ms frame
/// budget, so the crossover lines up naturally.
const GRAPH_WORKER_NODE_THRESHOLD = 3000;
/// Above this node count, pointer interactions must not repaint graph-wide
/// neighborhoods through Sigma. Dense graphs use DOM/canvas affordances for
/// hover and incident drag edges; the only live Sigma repaint allowed during
/// drag is the single node under the cursor.
const DOM_ONLY_INTERACTION_NODE_THRESHOLD = 15000;
/// Local edge affordance cap for the DOM/canvas overlay. The full topology
/// remains available to the inspector, but drawing tens of thousands of
/// incident lines for one hub on every drag/camera frame would move the same
/// Firefox bottleneck into 2D canvas.
const NEIGHBORHOOD_OVERLAY_EDGE_LIMIT = 1200;
const DRAG_NEIGHBORHOOD_OVERLAY_EDGE_LIMIT = 320;
const BASELINE_EDGE_ENDPOINT_COVERAGE_RATIO = 0.6;
const BASELINE_EDGE_LOCAL_DETAIL_RATIO = 0.9;
const ALL_EDGES_LAYER_NODE_THRESHOLD = 15000;
const ALL_EDGES_LAYER_EDGE_THRESHOLD = GRAPH_EDGE_RENDER_CAP;
const ALL_EDGES_LAYER_DARK_COLOR: readonly [number, number, number, number] = [0.78, 0.84, 0.92, 0.22];
const ALL_EDGES_LAYER_LIGHT_COLOR: readonly [number, number, number, number] = [0.06, 0.1, 0.18, 0.34];
const EDGE_PAIR_KEY_SEPARATOR = '\u001f';

function edgePairKey(sourceId: string, targetId: string): string {
  return `${sourceId}${EDGE_PAIR_KEY_SEPARATOR}${targetId}`;
}

function hashSamplePart(hash: number, value: string): number {
  let next = hash;
  for (let i = 0; i < value.length; i += 1) {
    next ^= value.charCodeAt(i);
    next = Math.imul(next, 16777619);
  }
  return next >>> 0;
}

function edgeSampleHash(edge: EdgeData): number {
  let hash = 2166136261;
  hash = hashSamplePart(hash, edge.id);
  hash = hashSamplePart(hash, edge.sourceId);
  hash = hashSamplePart(hash, edge.targetId);
  return hash >>> 0;
}

function baselineEdgeSampleQuota(degree: number): number {
  if (degree >= 1024) return 40;
  if (degree >= 512) return 32;
  if (degree >= 256) return 24;
  if (degree >= 128) return 18;
  if (degree >= 64) return 14;
  if (degree >= 16) return 10;
  if (degree >= 4) return 5;
  return 2;
}

function applyCheapFallbackLayout(graph: Graph): void {
  const order = graph.order;
  if (order === 0) return;

  const columns = Math.max(1, Math.ceil(Math.sqrt(order)));
  const rows = Math.max(1, Math.ceil(order / columns));
  const gap = Math.max(8, Math.sqrt(order) * 0.8);
  let index = 0;

  graph.updateEachNodeAttributes(
    (_id, attr) => {
      const row = Math.floor(index / columns);
      const column = index % columns;
      attr.x = (column - (columns - 1) / 2) * gap;
      attr.y = (row - (rows - 1) / 2) * gap;
      index += 1;
      return attr;
    },
    { attributes: ['x', 'y'] },
  );
}

function getCanvas2dContext(canvas: HTMLCanvasElement): CanvasRenderingContext2D | null {
  // JSDOM exposes getContext but only reports "not implemented" through its
  // virtual console. The overlay is visual-only, so unit tests can skip it.
  if (
    typeof window !== 'undefined' &&
    window.navigator.userAgent.toLowerCase().includes('jsdom')
  ) {
    return null;
  }
  return canvas.getContext('2d');
}

function compileAllEdgesShader(
  gl: WebGLRenderingContext | WebGL2RenderingContext,
  type: number,
  source: string,
): WebGLShader | null {
  const shader = gl.createShader(type);
  if (!shader) return null;
  gl.shaderSource(shader, source);
  gl.compileShader(shader);
  if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) {
    gl.deleteShader(shader);
    return null;
  }
  return shader;
}

function createIndexedAllEdgesLayerState(canvas: HTMLCanvasElement): AllEdgesLayerState | null {
  if (
    typeof window !== 'undefined' &&
    window.navigator.userAgent.toLowerCase().includes('jsdom')
  ) {
    return null;
  }

  const gl = canvas.getContext('webgl2', {
    alpha: true,
    antialias: false,
    depth: false,
    preserveDrawingBuffer: false,
    premultipliedAlpha: true,
    stencil: false,
  });
  if (!gl) return null;
  const coordinateFallback = () => createCoordinateAllEdgesLayerState(canvas, gl);

  const vertexShader = compileAllEdgesShader(
    gl,
    gl.VERTEX_SHADER,
    `#version 300 es
      precision highp float;

      in vec3 a_edgeData;
      uniform mat3 u_matrix;
      uniform sampler2D u_positionTexture;
      uniform float u_positionTextureWidth;

      vec2 readPosition(float nodeIndex) {
        float texX = mod(nodeIndex, u_positionTextureWidth);
        float texY = floor(nodeIndex / u_positionTextureWidth);
        return texelFetch(u_positionTexture, ivec2(int(texX), int(texY)), 0).xy;
      }

      void main() {
        float endpointIndex = mix(a_edgeData.x, a_edgeData.y, step(0.5, a_edgeData.z));
        vec2 graphPosition = readPosition(endpointIndex);
        vec3 position = u_matrix * vec3(graphPosition, 1.0);
        gl_Position = vec4(position.xy, 0.0, 1.0);
      }
    `,
  );
  const fragmentShader = compileAllEdgesShader(
    gl,
    gl.FRAGMENT_SHADER,
    `#version 300 es
      precision mediump float;

      uniform vec4 u_color;
      out vec4 outColor;

      void main() {
        outColor = u_color;
      }
    `,
  );
  if (!vertexShader || !fragmentShader) {
    if (vertexShader) gl.deleteShader(vertexShader);
    if (fragmentShader) gl.deleteShader(fragmentShader);
    return coordinateFallback();
  }

  const program = gl.createProgram();
  const edgeBuffer = gl.createBuffer();
  const positionTexture = gl.createTexture();
  if (!program || !edgeBuffer || !positionTexture) {
    if (program) gl.deleteProgram(program);
    if (edgeBuffer) gl.deleteBuffer(edgeBuffer);
    if (positionTexture) gl.deleteTexture(positionTexture);
    return coordinateFallback();
  }
  gl.attachShader(program, vertexShader);
  gl.attachShader(program, fragmentShader);
  gl.linkProgram(program);
  gl.deleteShader(vertexShader);
  gl.deleteShader(fragmentShader);
  if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
    gl.deleteProgram(program);
    gl.deleteBuffer(edgeBuffer);
    gl.deleteTexture(positionTexture);
    return coordinateFallback();
  }

  const edgeDataLocation = gl.getAttribLocation(program, 'a_edgeData');
  const matrixLocation = gl.getUniformLocation(program, 'u_matrix');
  const colorLocation = gl.getUniformLocation(program, 'u_color');
  const positionTextureLocation = gl.getUniformLocation(program, 'u_positionTexture');
  const positionTextureWidthLocation = gl.getUniformLocation(program, 'u_positionTextureWidth');
  if (
    edgeDataLocation < 0 ||
    !matrixLocation ||
    !colorLocation ||
    !positionTextureLocation ||
    !positionTextureWidthLocation
  ) {
    gl.deleteProgram(program);
    gl.deleteBuffer(edgeBuffer);
    gl.deleteTexture(positionTexture);
    return coordinateFallback();
  }

  gl.bindTexture(gl.TEXTURE_2D, positionTexture);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
  gl.disable(gl.DEPTH_TEST);
  gl.enable(gl.BLEND);
  gl.blendFunc(gl.SRC_ALPHA, gl.ONE_MINUS_SRC_ALPHA);

  return {
    kind: 'indexed',
    gl,
    program,
    edgeBuffer,
    positionTexture,
    edgeDataLocation,
    matrixLocation,
    colorLocation,
    positionTextureLocation,
    positionTextureWidthLocation,
    nodeIndexById: new Map(),
    positionTextureWidth: 1,
    positionTextureHeight: 1,
    positionTextureData: new Float32Array(4),
    scratchTexel: new Float32Array(4),
  };
}

function createCoordinateAllEdgesLayerState(
  canvas: HTMLCanvasElement,
  existingGl?: WebGLRenderingContext | WebGL2RenderingContext,
): AllEdgesCoordinateLayerState | null {
  if (
    typeof window !== 'undefined' &&
    window.navigator.userAgent.toLowerCase().includes('jsdom')
  ) {
    return null;
  }

  const gl =
    existingGl ??
    canvas.getContext('webgl', {
      alpha: true,
      antialias: false,
      depth: false,
      preserveDrawingBuffer: false,
      premultipliedAlpha: true,
      stencil: false,
    });
  if (!gl) return null;
  const isWebGl2 = typeof WebGL2RenderingContext !== 'undefined' && gl instanceof WebGL2RenderingContext;

  const vertexShader = compileAllEdgesShader(
    gl,
    gl.VERTEX_SHADER,
    isWebGl2
      ? `#version 300 es
        precision highp float;

        in vec2 a_position;
        uniform mat3 u_matrix;

        void main() {
          vec3 position = u_matrix * vec3(a_position, 1.0);
          gl_Position = vec4(position.xy, 0.0, 1.0);
        }
      `
      : `
        attribute vec2 a_position;
        uniform mat3 u_matrix;

        void main() {
          vec3 position = u_matrix * vec3(a_position, 1.0);
          gl_Position = vec4(position.xy, 0.0, 1.0);
        }
      `,
  );
  const fragmentShader = compileAllEdgesShader(
    gl,
    gl.FRAGMENT_SHADER,
    isWebGl2
      ? `#version 300 es
        precision mediump float;

        uniform vec4 u_color;
        out vec4 outColor;

        void main() {
          outColor = u_color;
        }
      `
      : `
        precision mediump float;
        uniform vec4 u_color;

        void main() {
          gl_FragColor = u_color;
        }
      `,
  );
  if (!vertexShader || !fragmentShader) return null;

  const program = gl.createProgram();
  const buffer = gl.createBuffer();
  if (!program || !buffer) return null;
  gl.attachShader(program, vertexShader);
  gl.attachShader(program, fragmentShader);
  gl.linkProgram(program);
  gl.deleteShader(vertexShader);
  gl.deleteShader(fragmentShader);
  if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
    gl.deleteProgram(program);
    gl.deleteBuffer(buffer);
    return null;
  }

  const positionLocation = gl.getAttribLocation(program, 'a_position');
  const matrixLocation = gl.getUniformLocation(program, 'u_matrix');
  const colorLocation = gl.getUniformLocation(program, 'u_color');
  if (positionLocation < 0 || !matrixLocation || !colorLocation) {
    gl.deleteProgram(program);
    gl.deleteBuffer(buffer);
    return null;
  }

  gl.disable(gl.DEPTH_TEST);
  gl.enable(gl.BLEND);
  gl.blendFunc(gl.SRC_ALPHA, gl.ONE_MINUS_SRC_ALPHA);

  return {
    kind: 'coordinate',
    gl,
    program,
    buffer,
    positionLocation,
    matrixLocation,
    colorLocation,
  };
}

function createAllEdgesLayerState(canvas: HTMLCanvasElement): AllEdgesLayerState | null {
  return createIndexedAllEdgesLayerState(canvas) ?? createCoordinateAllEdgesLayerState(canvas);
}

// --- Component ---

function SigmaGraph({ nodes, edges, selectedId, onSelect, layout, hiddenIds, onFitViewReady, showDenseEdges = false }: SigmaGraphProps) {
  const { t } = useTranslation();
  const preferences = useContext(PreferencesContext);
  const resolvedTheme =
    preferences?.resolvedTheme ??
    (typeof document !== 'undefined' && document.documentElement.classList.contains('dark') ? 'dark' : 'light');
  const containerRef = useRef<HTMLDivElement>(null);
  const tooltipRef = useRef<HTMLDivElement>(null);
  const sigmaRef = useRef<Sigma | null>(null);
  const [graphInstanceVersion, setGraphInstanceVersion] = useState(0);
  // Keep a stable ref to onFitViewReady so it can be read inside the
  // sigma-creation effect without appearing in the dependency array.
  // Adding the callback itself to deps would recreate the entire Sigma
  // instance on every parent render that passes a new function reference.
  const onFitViewReadyRef = useRef(onFitViewReady);
  useLayoutEffect(() => {
    onFitViewReadyRef.current = onFitViewReady;
  });
  // Same stability requirement as `onFitViewReady`: the parent updates the URL
  // query string on selection, so its callback identity can change even though
  // Sigma's topology has not. Reading through a ref keeps selection from
  // tearing down/recreating the renderer and recentering the camera.
  const onSelectRef = useRef(onSelect);
  useLayoutEffect(() => {
    onSelectRef.current = onSelect;
  });
  const graphRef = useRef<Graph | null>(null);
  const edgeLodExtraEdgeIdsRef = useRef<Set<string>>(new Set());
  const lastTopologyRef = useRef<{ nodes: GraphNode[]; edges: EdgeData[] } | null>(null);
  const lastCameraStateRef = useRef<CameraState | null>(null);
  const dragStateRef = useRef<{ dragging: boolean; node: string | null }>({ dragging: false, node: null });
  const selectedIdRef = useRef(selectedId);
  const [hoveredId, setHoveredId] = useState<string | null>(null);
  const layoutRef = useRef(layout);
  const layoutAnimationFrameRef = useRef<number | null>(null);
  const layoutAnimationTokenRef = useRef(0);
  // Monotonic token guarding async layout-switch recomputes. Bumped on
  // every switch so a stale worker result (user toggled twice quickly)
  // can be discarded before it touches Sigma.
  const layoutSwitchTokenRef = useRef(0);
  // Slim, transferable-friendly payload describing the CURRENT topology,
  // reused by the layout-switch effect so it never has to clone the live
  // Graphology instance (a ~770 ms main-thread stall at 100k nodes) just
  // to feed the layout worker. Rebuilt once per (nodes, edges) inside the
  // build effect, exactly mirroring the payload the initial render sends.
  const workerPayloadRef = useRef<{
    nodes: Array<{ id: string; nodeType: string; size: number; label: string }>;
    edges: Array<{ sourceId: string; targetId: string }>;
  } | null>(null);
  // Surfaced to a lightweight "recomputing layout" affordance while a
  // heavy async layout is in flight, so switching modes on a 100k-node
  // graph reads as deliberate rather than frozen.
  const [layoutRecomputing, setLayoutRecomputing] = useState(false);
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
  const neighborIndexRef = useRef(neighborIndex);
  useLayoutEffect(() => {
    neighborIndexRef.current = neighborIndex;
  }, [neighborIndex]);

  const hiddenIdsRef = useRef(hiddenIds);
  useLayoutEffect(() => {
    hiddenIdsRef.current = hiddenIds;
  }, [hiddenIds]);

  useLayoutEffect(() => {
    selectedIdRef.current = selectedId;
  }, [selectedId]);

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

  // Tracks the node/edge ids that the PREVIOUS reducer run visually
  // touched (hovered/selected node + its neighbors + incident edges).
  // On the next hover/selection transition we partial-refresh the UNION
  // of {previously-affected} ∪ {newly-affected} so Sigma re-applies the
  // reducers to ~O(degree) elements instead of re-running them across
  // ALL nodes/edges. A full `sigma.refresh()` is O(N): on a 25k-node /
  // 160k-edge graph it blocks the main thread ~120 ms the instant the
  // cursor stops on a node, which is the perceived "freeze". The partial
  // path drops that to O(deg(hovered) + deg(prev)) — a handful of
  // updateNode/updateEdge calls — so the transition is imperceptible.
  const affectedNodeIdsRef = useRef<Set<string>>(new Set());
  const affectedEdgeIdsRef = useRef<Set<string>>(new Set());
  const renderedEdgeIdsRef = useRef<string[]>([]);

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
  const dragPreviewRef = useRef<HTMLDivElement>(null);
  const dragPreviewDotRef = useRef<HTMLSpanElement>(null);
  const dragPreviewLabelRef = useRef<HTMLSpanElement>(null);
  const allEdgesCanvasRef = useRef<HTMLCanvasElement>(null);
  const allEdgesLayerStateRef = useRef<AllEdgesLayerState | null>(null);
  const allEdgesLayerFrameRef = useRef<number | null>(null);
  const allEdgesLayerVertexCountRef = useRef(0);
  const allEdgesLayerEnabledRef = useRef(false);
  const suspendAllEdgesLayerAutoDrawRef = useRef(false);
  const suspendAllEdgesLayerOwnerTokenRef = useRef<number | null>(null);
  const neighborhoodCanvasRef = useRef<HTMLCanvasElement>(null);
  const neighborhoodOverlayFocusRef = useRef<NeighborhoodOverlayFocus>(null);
  const neighborhoodOverlayFrameRef = useRef<number | null>(null);
  const dragOverlayTargetsRef = useRef<Array<{ x: number; y: number }> | null>(null);
  const dragOverlaySourceRef = useRef<{ x: number; y: number } | null>(null);
  const clearAllEdgesLayer = useCallback(() => {
    allEdgesLayerEnabledRef.current = false;
    const canvas = allEdgesCanvasRef.current;
    const state = allEdgesLayerStateRef.current;
    if (!canvas || !state) return;
    allEdgesLayerVertexCountRef.current = 0;
    const gl = state.gl;
    gl.viewport(0, 0, canvas.width, canvas.height);
    gl.clearColor(0, 0, 0, 0);
    gl.clear(gl.COLOR_BUFFER_BIT);
    canvas.removeAttribute('data-all-edges-count');
    canvas.removeAttribute('data-all-edges-layer');
  }, []);
  const drawAllEdgesLayerNow = useCallback(() => {
    const canvas = allEdgesCanvasRef.current;
    const sigma = sigmaRef.current;
    const container = containerRef.current;
    const state = allEdgesLayerStateRef.current;
    const vertexCount = allEdgesLayerVertexCountRef.current;
    if (!canvas || !sigma || !container || !state || !allEdgesLayerEnabledRef.current || vertexCount <= 0) {
      return;
    }

    const rect = container.getBoundingClientRect();
    if (rect.width <= 0 || rect.height <= 0) return;

    const pixelRatio = Math.min(window.devicePixelRatio || 1, 2);
    const nextWidth = Math.max(1, Math.floor(rect.width * pixelRatio));
    const nextHeight = Math.max(1, Math.floor(rect.height * pixelRatio));
    if (canvas.width !== nextWidth || canvas.height !== nextHeight) {
      canvas.width = nextWidth;
      canvas.height = nextHeight;
    }
    canvas.style.width = `${rect.width}px`;
    canvas.style.height = `${rect.height}px`;

    const origin = sigma.graphToViewport({ x: 0, y: 0 });
    const unitX = sigma.graphToViewport({ x: 1, y: 0 });
    const unitY = sigma.graphToViewport({ x: 0, y: 1 });
    const a = unitX.x - origin.x;
    const b = unitX.y - origin.y;
    const c = unitY.x - origin.x;
    const d = unitY.y - origin.y;
    const e = origin.x;
    const f = origin.y;
    const matrix = new Float32Array([
      (2 * a) / rect.width,
      (-2 * b) / rect.height,
      0,
      (2 * c) / rect.width,
      (-2 * d) / rect.height,
      0,
      (2 * e) / rect.width - 1,
      1 - (2 * f) / rect.height,
      1,
    ]);
    const color = resolvedTheme === 'dark' ? ALL_EDGES_LAYER_DARK_COLOR : ALL_EDGES_LAYER_LIGHT_COLOR;
    const gl = state.gl;
    gl.viewport(0, 0, canvas.width, canvas.height);
    gl.clearColor(0, 0, 0, 0);
    gl.clear(gl.COLOR_BUFFER_BIT);
    gl.useProgram(state.program);
    if (state.kind === 'indexed') {
      gl.activeTexture(gl.TEXTURE0);
      gl.bindTexture(gl.TEXTURE_2D, state.positionTexture);
      gl.uniform1i(state.positionTextureLocation, 0);
      gl.uniform1f(state.positionTextureWidthLocation, state.positionTextureWidth);
      gl.bindBuffer(gl.ARRAY_BUFFER, state.edgeBuffer);
      gl.enableVertexAttribArray(state.edgeDataLocation);
      gl.vertexAttribPointer(state.edgeDataLocation, 3, gl.FLOAT, false, 0, 0);
    } else {
      gl.bindBuffer(gl.ARRAY_BUFFER, state.buffer);
      gl.enableVertexAttribArray(state.positionLocation);
      gl.vertexAttribPointer(state.positionLocation, 2, gl.FLOAT, false, 0, 0);
    }
    gl.uniformMatrix3fv(state.matrixLocation, false, matrix);
    gl.uniform4f(state.colorLocation, color[0], color[1], color[2], color[3]);
    gl.drawArrays(gl.LINES, 0, vertexCount);
  }, [resolvedTheme]);
  const scheduleAllEdgesLayerDraw = useCallback(() => {
    if (suspendAllEdgesLayerAutoDrawRef.current) return;
    if (allEdgesLayerFrameRef.current != null) return;
    allEdgesLayerFrameRef.current = requestAnimationFrame(() => {
      allEdgesLayerFrameRef.current = null;
      drawAllEdgesLayerNow();
    });
  }, [drawAllEdgesLayerNow]);
  const suspendAllEdgesLayerDraws = useCallback((token: number) => {
    suspendAllEdgesLayerAutoDrawRef.current = true;
    suspendAllEdgesLayerOwnerTokenRef.current = token;
    if (allEdgesLayerFrameRef.current != null) {
      cancelAnimationFrame(allEdgesLayerFrameRef.current);
      allEdgesLayerFrameRef.current = null;
    }
    const canvas = allEdgesCanvasRef.current;
    if (canvas) canvas.style.visibility = 'hidden';
  }, []);
  const resumeAllEdgesLayerDraws = useCallback((token: number, drawFresh: boolean) => {
    if (suspendAllEdgesLayerOwnerTokenRef.current !== token) return false;
    suspendAllEdgesLayerOwnerTokenRef.current = null;
    suspendAllEdgesLayerAutoDrawRef.current = false;
    if (drawFresh) drawAllEdgesLayerNow();
    const canvas = allEdgesCanvasRef.current;
    if (canvas) canvas.style.visibility = '';
    return true;
  }, [drawAllEdgesLayerNow]);
  const forceClearAllEdgesLayerSuspension = useCallback(() => {
    suspendAllEdgesLayerOwnerTokenRef.current = null;
    suspendAllEdgesLayerAutoDrawRef.current = false;
    if (allEdgesLayerFrameRef.current != null) {
      cancelAnimationFrame(allEdgesLayerFrameRef.current);
      allEdgesLayerFrameRef.current = null;
    }
    const canvas = allEdgesCanvasRef.current;
    if (canvas) canvas.style.visibility = '';
  }, []);
  const rebuildAllEdgesLayer = useCallback((graph: Graph, visibleEdges: EdgeData[], enabled: boolean) => {
    const canvas = allEdgesCanvasRef.current;
    if (!canvas || !enabled) {
      clearAllEdgesLayer();
      return;
    }

    let state = allEdgesLayerStateRef.current;
    if (!state) {
      state = createAllEdgesLayerState(canvas);
      allEdgesLayerStateRef.current = state;
    }
    if (!state) return;

    const hidden = hiddenIdsRef.current;

    if (state.kind === 'indexed') {
      const gl = state.gl;
      const nodeIndexById = new Map<string, number>();
      graph.forEachNode((node, attr) => {
        if (hidden?.has(node)) return;
        const x = attr.x as number | undefined;
        const y = attr.y as number | undefined;
        if (x == null || y == null) return;
        nodeIndexById.set(node, nodeIndexById.size);
      });

      const nodeCount = nodeIndexById.size;
      if (nodeCount === 0) {
        allEdgesLayerVertexCountRef.current = 0;
        allEdgesLayerEnabledRef.current = false;
        gl.clearColor(0, 0, 0, 0);
        gl.clear(gl.COLOR_BUFFER_BIT);
        return;
      }

      const maxTextureSize = gl.getParameter(gl.MAX_TEXTURE_SIZE) as number;
      const positionTextureWidth = Math.min(maxTextureSize, Math.max(1, Math.ceil(Math.sqrt(nodeCount))));
      const positionTextureHeight = Math.ceil(nodeCount / positionTextureWidth);
      if (positionTextureHeight > maxTextureSize) {
        state = createCoordinateAllEdgesLayerState(canvas, gl);
        allEdgesLayerStateRef.current = state;
        if (!state) return;
      } else {
        const positionTextureData = new Float32Array(positionTextureWidth * positionTextureHeight * 4);
        graph.forEachNode((node, attr) => {
          const index = nodeIndexById.get(node);
          if (index == null) return;
          const offset = index * 4;
          positionTextureData[offset] = (attr.x as number | undefined) ?? 0;
          positionTextureData[offset + 1] = (attr.y as number | undefined) ?? 0;
        });

        const edgeData = new Float32Array(visibleEdges.length * 6);
        let edgeOffset = 0;
        let edgeCount = 0;
        for (const edge of visibleEdges) {
          if (edge.sourceId === edge.targetId) continue;
          if (hidden?.has(edge.sourceId) || hidden?.has(edge.targetId)) continue;
          const sourceIndex = nodeIndexById.get(edge.sourceId);
          const targetIndex = nodeIndexById.get(edge.targetId);
          if (sourceIndex == null || targetIndex == null) continue;
          edgeData[edgeOffset] = sourceIndex;
          edgeData[edgeOffset + 1] = targetIndex;
          edgeData[edgeOffset + 2] = 0;
          edgeData[edgeOffset + 3] = sourceIndex;
          edgeData[edgeOffset + 4] = targetIndex;
          edgeData[edgeOffset + 5] = 1;
          edgeOffset += 6;
          edgeCount += 1;
        }

        gl.bindTexture(gl.TEXTURE_2D, state.positionTexture);
        gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1);
        gl.texImage2D(
          gl.TEXTURE_2D,
          0,
          gl.RGBA32F,
          positionTextureWidth,
          positionTextureHeight,
          0,
          gl.RGBA,
          gl.FLOAT,
          positionTextureData,
        );
        if (gl.getError() !== gl.NO_ERROR) {
          const fallbackState = createCoordinateAllEdgesLayerState(canvas, gl);
          allEdgesLayerStateRef.current = fallbackState;
          if (!fallbackState) {
            clearAllEdgesLayer();
            return;
          }
          state = fallbackState;
        } else {
          gl.bindBuffer(gl.ARRAY_BUFFER, state.edgeBuffer);
          gl.bufferData(
            gl.ARRAY_BUFFER,
            edgeOffset === edgeData.length ? edgeData : edgeData.subarray(0, edgeOffset),
            gl.STATIC_DRAW,
          );

          const nextState: AllEdgesIndexedLayerState = {
            ...state,
            nodeIndexById,
            positionTextureWidth,
            positionTextureHeight,
            positionTextureData,
          };
          allEdgesLayerStateRef.current = nextState;
          allEdgesLayerVertexCountRef.current = edgeCount * 2;
          allEdgesLayerEnabledRef.current = edgeCount > 0;
          canvas.dataset.allEdgesCount = String(edgeCount);
          canvas.dataset.allEdgesLayer = 'indexed';
          if (edgeCount > 0) {
            scheduleAllEdgesLayerDraw();
          } else {
            gl.viewport(0, 0, canvas.width, canvas.height);
            gl.clearColor(0, 0, 0, 0);
            gl.clear(gl.COLOR_BUFFER_BIT);
          }
          return;
        }
      }
    }

    if (state.kind !== 'coordinate') return;
    const vertices = new Float32Array(visibleEdges.length * 4);
    let offset = 0;
    let edgeCount = 0;
    for (const edge of visibleEdges) {
      if (edge.sourceId === edge.targetId) continue;
      if (hidden?.has(edge.sourceId) || hidden?.has(edge.targetId)) continue;
      if (!graph.hasNode(edge.sourceId) || !graph.hasNode(edge.targetId)) continue;
      const sourceX = graph.getNodeAttribute(edge.sourceId, 'x') as number | undefined;
      const sourceY = graph.getNodeAttribute(edge.sourceId, 'y') as number | undefined;
      const targetX = graph.getNodeAttribute(edge.targetId, 'x') as number | undefined;
      const targetY = graph.getNodeAttribute(edge.targetId, 'y') as number | undefined;
      if (sourceX == null || sourceY == null || targetX == null || targetY == null) continue;
      vertices[offset] = sourceX;
      vertices[offset + 1] = sourceY;
      vertices[offset + 2] = targetX;
      vertices[offset + 3] = targetY;
      offset += 4;
      edgeCount += 1;
    }

    const gl = state.gl;
    gl.bindBuffer(gl.ARRAY_BUFFER, state.buffer);
    gl.bufferData(gl.ARRAY_BUFFER, offset === vertices.length ? vertices : vertices.subarray(0, offset), gl.STATIC_DRAW);
    allEdgesLayerVertexCountRef.current = edgeCount * 2;
    allEdgesLayerEnabledRef.current = edgeCount > 0;
    canvas.dataset.allEdgesCount = String(edgeCount);
    canvas.dataset.allEdgesLayer = 'coordinate';
    if (edgeCount > 0) {
      scheduleAllEdgesLayerDraw();
    } else {
      gl.viewport(0, 0, canvas.width, canvas.height);
      gl.clearColor(0, 0, 0, 0);
      gl.clear(gl.COLOR_BUFFER_BIT);
    }
  }, [clearAllEdgesLayer, scheduleAllEdgesLayerDraw]);
  const updateAllEdgesLayerNodePosition = useCallback((node: string, position: { x: number; y: number }) => {
    const state = allEdgesLayerStateRef.current;
    if (!state || state.kind !== 'indexed' || !allEdgesLayerEnabledRef.current) return false;
    const nodeIndex = state.nodeIndexById.get(node);
    if (nodeIndex == null) return false;

    const offset = nodeIndex * 4;
    state.positionTextureData[offset] = position.x;
    state.positionTextureData[offset + 1] = position.y;
    state.scratchTexel[0] = position.x;
    state.scratchTexel[1] = position.y;
    state.scratchTexel[2] = 0;
    state.scratchTexel[3] = 0;

    const x = nodeIndex % state.positionTextureWidth;
    const y = Math.floor(nodeIndex / state.positionTextureWidth);
    const gl = state.gl;
    gl.bindTexture(gl.TEXTURE_2D, state.positionTexture);
    gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1);
    gl.texSubImage2D(gl.TEXTURE_2D, 0, x, y, 1, 1, gl.RGBA, gl.FLOAT, state.scratchTexel);
    scheduleAllEdgesLayerDraw();
    return true;
  }, [scheduleAllEdgesLayerDraw]);
  const uploadAllEdgesLayerPositionTexture = useCallback((state: AllEdgesIndexedLayerState) => {
    const gl = state.gl;
    gl.bindTexture(gl.TEXTURE_2D, state.positionTexture);
    gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1);
    gl.texSubImage2D(
      gl.TEXTURE_2D,
      0,
      0,
      0,
      state.positionTextureWidth,
      state.positionTextureHeight,
      gl.RGBA,
      gl.FLOAT,
      state.positionTextureData,
    );
    scheduleAllEdgesLayerDraw();
    return true;
  }, [scheduleAllEdgesLayerDraw]);
  const uploadAllEdgesLayerPositionTextureChunked = useCallback(async (
    state: AllEdgesIndexedLayerState,
    isCurrent: () => boolean,
  ) => {
    const gl = state.gl;
    const rowsPerChunk = state.positionTextureHeight >= 64 ? 4 : state.positionTextureHeight;
    gl.bindTexture(gl.TEXTURE_2D, state.positionTexture);
    gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1);

    for (let row = 0; row < state.positionTextureHeight; row += rowsPerChunk) {
      if (!isCurrent() || allEdgesLayerStateRef.current !== state) return false;
      const rowCount = Math.min(rowsPerChunk, state.positionTextureHeight - row);
      const start = row * state.positionTextureWidth * 4;
      const end = (row + rowCount) * state.positionTextureWidth * 4;
      gl.texSubImage2D(
        gl.TEXTURE_2D,
        0,
        0,
        row,
        state.positionTextureWidth,
        rowCount,
        gl.RGBA,
        gl.FLOAT,
        state.positionTextureData.subarray(start, end),
      );
      await new Promise<void>((resolve) => {
        requestAnimationFrame(() => resolve());
      });
    }

    if (!isCurrent() || allEdgesLayerStateRef.current !== state) return false;
    scheduleAllEdgesLayerDraw();
    return true;
  }, [scheduleAllEdgesLayerDraw]);
  const syncAllEdgesLayerPositionsFromGraph = useCallback((graph: Graph, enabled: boolean) => {
    const state = allEdgesLayerStateRef.current;
    if (!enabled || !state || state.kind !== 'indexed' || !allEdgesLayerEnabledRef.current) return false;

    for (const [node, nodeIndex] of state.nodeIndexById) {
      if (!graph.hasNode(node)) continue;
      const x = graph.getNodeAttribute(node, 'x') as number | undefined;
      const y = graph.getNodeAttribute(node, 'y') as number | undefined;
      if (x == null || y == null) continue;
      const offset = nodeIndex * 4;
      state.positionTextureData[offset] = x;
      state.positionTextureData[offset + 1] = y;
    }

    return uploadAllEdgesLayerPositionTexture(state);
  }, [uploadAllEdgesLayerPositionTexture]);
  const writeAllEdgesLayerPositionsFromLayoutChunked = useCallback(async (
    layoutNodes: Array<{ id: string }>,
    positions: ArrayLike<number>,
    enabled: boolean,
    isCurrent: () => boolean,
  ) => {
    const state = allEdgesLayerStateRef.current;
    if (!enabled || !state || state.kind !== 'indexed' || !allEdgesLayerEnabledRef.current) return null;
    const chunkSize = layoutNodes.length >= 50000 ? 1000 : 5000;

    for (let sourceIndex = 0; sourceIndex < layoutNodes.length; sourceIndex += chunkSize) {
      if (!isCurrent() || allEdgesLayerStateRef.current !== state) return null;
      const limit = Math.min(layoutNodes.length, sourceIndex + chunkSize);
      for (let index = sourceIndex; index < limit; index += 1) {
        const nodeIndex = state.nodeIndexById.get(layoutNodes[index].id);
        if (nodeIndex == null) continue;
        const offset = nodeIndex * 4;
        state.positionTextureData[offset] = positions[index * 2] ?? 0;
        state.positionTextureData[offset + 1] = positions[index * 2 + 1] ?? 0;
      }
      await new Promise<void>((resolve) => {
        requestAnimationFrame(() => resolve());
      });
    }

    if (!isCurrent() || allEdgesLayerStateRef.current !== state) return null;
    return state;
  }, []);
  const syncOrRebuildAllEdgesLayerPositions = useCallback((graph: Graph, visibleEdges: EdgeData[], enabled: boolean) => {
    if (syncAllEdgesLayerPositionsFromGraph(graph, enabled)) return;
    rebuildAllEdgesLayer(graph, visibleEdges, enabled);
  }, [rebuildAllEdgesLayer, syncAllEdgesLayerPositionsFromGraph]);
  const clearNeighborhoodOverlay = useCallback(() => {
    const canvas = neighborhoodCanvasRef.current;
    if (!canvas) return;
    canvas.removeAttribute('data-overlay-node-id');
    canvas.removeAttribute('data-overlay-edge-count');
    canvas.removeAttribute('data-overlay-source-x');
    canvas.removeAttribute('data-overlay-source-y');
    const context = getCanvas2dContext(canvas);
    if (!context) return;
    context.setTransform(1, 0, 0, 1, 0, 0);
    context.clearRect(0, 0, canvas.width, canvas.height);
  }, []);
  const drawNeighborhoodOverlayNow = useCallback(() => {
    const focus = neighborhoodOverlayFocusRef.current;
    const canvas = neighborhoodCanvasRef.current;
    const sigma = sigmaRef.current;
    const graph = graphRef.current;
    const container = containerRef.current;
    if (!focus || !canvas || !sigma || !graph || !container || !graph.hasNode(focus.nodeId)) {
      clearNeighborhoodOverlay();
      return;
    }

    const context = getCanvas2dContext(canvas);
    if (!context) return;

    const rect = container.getBoundingClientRect();
    if (rect.width <= 0 || rect.height <= 0) {
      clearNeighborhoodOverlay();
      return;
    }

    const pixelRatio = Math.min(window.devicePixelRatio || 1, 2);
    const nextWidth = Math.max(1, Math.floor(rect.width * pixelRatio));
    const nextHeight = Math.max(1, Math.floor(rect.height * pixelRatio));
    if (canvas.width !== nextWidth || canvas.height !== nextHeight) {
      canvas.width = nextWidth;
      canvas.height = nextHeight;
    }
    canvas.style.width = `${rect.width}px`;
    canvas.style.height = `${rect.height}px`;

    context.setTransform(1, 0, 0, 1, 0, 0);
    context.clearRect(0, 0, canvas.width, canvas.height);
    context.setTransform(pixelRatio, 0, 0, pixelRatio, 0, 0);

    const hidden = hiddenIdsRef.current;
    const dragSource = focus.mode === 'drag' ? dragOverlaySourceRef.current : null;
    let source: { x: number; y: number };
    if (dragSource) {
      source = dragSource;
    } else {
      const x = graph.getNodeAttribute(focus.nodeId, 'x') as number | undefined;
      const y = graph.getNodeAttribute(focus.nodeId, 'y') as number | undefined;
      if (x == null || y == null) {
        clearNeighborhoodOverlay();
        return;
      }
      source = sigma.graphToViewport({ x, y });
    }
    const neighbors = neighborIndexRef.current.get(focus.nodeId) ?? new Set<string>();
    const stroke =
      focus.mode === 'drag'
        ? 'rgba(251, 191, 36, 0.84)'
        : focus.mode === 'selected'
          ? 'rgba(245, 158, 11, 0.82)'
          : 'rgba(226, 232, 240, 0.48)';
    const halo =
      focus.mode === 'drag'
        ? 'rgba(251, 191, 36, 0.28)'
        : focus.mode === 'selected'
          ? 'rgba(245, 158, 11, 0.24)'
          : 'rgba(226, 232, 240, 0.14)';

    context.save();
    context.lineCap = 'round';
    context.lineJoin = 'round';
    context.strokeStyle = stroke;
    context.lineWidth = focus.mode === 'hover' ? 1.05 : 1.55;
    context.globalCompositeOperation = 'source-over';

    let drawn = 0;
    const overlayEdgeLimit =
      focus.mode === 'drag' ? DRAG_NEIGHBORHOOD_OVERLAY_EDGE_LIMIT : NEIGHBORHOOD_OVERLAY_EDGE_LIMIT;
    const cachedDragTargets = focus.mode === 'drag' ? dragOverlayTargetsRef.current : null;
    context.beginPath();
    if (cachedDragTargets) {
      for (const target of cachedDragTargets) {
        if (drawn >= overlayEdgeLimit) break;
        context.moveTo(source.x, source.y);
        context.lineTo(target.x, target.y);
        drawn += 1;
      }
    } else {
      for (const neighbor of neighbors) {
        if (drawn >= overlayEdgeLimit) break;
        if (hidden?.has(neighbor) || !graph.hasNode(neighbor)) continue;
        const targetX = graph.getNodeAttribute(neighbor, 'x') as number | undefined;
        const targetY = graph.getNodeAttribute(neighbor, 'y') as number | undefined;
        if (targetX == null || targetY == null) continue;
        const target = sigma.graphToViewport({ x: targetX, y: targetY });
        context.moveTo(source.x, source.y);
        context.lineTo(target.x, target.y);
        drawn += 1;
      }
    }
    if (drawn > 0) context.stroke();

    context.fillStyle = halo;
    context.beginPath();
    context.arc(source.x, source.y, focus.mode === 'drag' ? 18 : 15, 0, Math.PI * 2);
    context.fill();
    context.strokeStyle =
      focus.mode === 'hover' ? 'rgba(226, 232, 240, 0.78)' : 'rgba(251, 191, 36, 0.94)';
    context.lineWidth = 2;
    context.beginPath();
    context.arc(source.x, source.y, focus.mode === 'drag' ? 8 : 7, 0, Math.PI * 2);
    context.stroke();
    context.restore();

    canvas.dataset.overlayNodeId = focus.nodeId;
    canvas.dataset.overlayEdgeCount = String(drawn);
    canvas.dataset.overlaySourceX = String(Math.round(source.x));
    canvas.dataset.overlaySourceY = String(Math.round(source.y));
  }, [clearNeighborhoodOverlay]);
  const scheduleNeighborhoodOverlayDraw = useCallback(() => {
    if (neighborhoodOverlayFrameRef.current != null) return;
    neighborhoodOverlayFrameRef.current = requestAnimationFrame(() => {
      neighborhoodOverlayFrameRef.current = null;
      drawNeighborhoodOverlayNow();
    });
  }, [drawNeighborhoodOverlayNow]);
  const hideDragPreview = useCallback(() => {
    const preview = dragPreviewRef.current;
    if (!preview) return;
    preview.hidden = true;
    preview.style.visibility = 'hidden';
    preview.style.transform = 'translate3d(-9999px, -9999px, 0)';
    preview.removeAttribute('data-drag-node-id');
  }, []);
  useEffect(() => {
    return () => {
      if (allEdgesLayerFrameRef.current != null) {
        cancelAnimationFrame(allEdgesLayerFrameRef.current);
        allEdgesLayerFrameRef.current = null;
      }
      if (neighborhoodOverlayFrameRef.current != null) {
        cancelAnimationFrame(neighborhoodOverlayFrameRef.current);
        neighborhoodOverlayFrameRef.current = null;
      }
    };
  }, []);

  useEffect(() => {
    const useDomOnlyInteractions = nodes.length >= DOM_ONLY_INTERACTION_NODE_THRESHOLD;
    if (!useDomOnlyInteractions) {
      neighborhoodOverlayFocusRef.current = null;
      clearNeighborhoodOverlay();
      return;
    }

    const current = neighborhoodOverlayFocusRef.current;
    if (current?.mode === 'drag') {
      scheduleNeighborhoodOverlayDraw();
      return;
    }

    if (selectedId && graphRef.current?.hasNode(selectedId)) {
      neighborhoodOverlayFocusRef.current = { nodeId: selectedId, mode: 'selected' };
    } else if (hoveredId && graphRef.current?.hasNode(hoveredId)) {
      neighborhoodOverlayFocusRef.current = { nodeId: hoveredId, mode: 'hover' };
    } else {
      neighborhoodOverlayFocusRef.current = null;
    }
    scheduleNeighborhoodOverlayDraw();
  }, [
    clearNeighborhoodOverlay,
    graphInstanceVersion,
    hiddenIds,
    hoveredId,
    nodes.length,
    scheduleNeighborhoodOverlayDraw,
    selectedId,
  ]);
  useLayoutEffect(() => {
    hideDragPreview();
  }, [hideDragPreview]);
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
    const previousGraph = graphRef.current;
    const canReuseLayout =
      previousGraph != null &&
      lastTopologyRef.current?.nodes === nodes &&
      lastTopologyRef.current?.edges === edges;
    const reusedPositions = new Map<string, { x: number; y: number }>();
    if (canReuseLayout && previousGraph) {
      for (const node of nodes) {
        if (!previousGraph.hasNode(node.id)) continue;
        const x = previousGraph.getNodeAttribute(node.id, 'x');
        const y = previousGraph.getNodeAttribute(node.id, 'y');
        if (typeof x === 'number' && typeof y === 'number') {
          reusedPositions.set(node.id, { x, y });
        }
      }
    }
    const reuseLayout = canReuseLayout && reusedPositions.size === nodes.length;
    const reusedCameraState = reuseLayout ? lastCameraStateRef.current : null;
    const graph = new Graph();

    const visibleNodes = nodes;
    const visibleNodeIds = new Set(visibleNodes.map(n => n.id));
    const visibleEdges = edges.filter((edge) =>
      edge.sourceId !== edge.targetId &&
      visibleNodeIds.has(edge.sourceId) &&
      visibleNodeIds.has(edge.targetId),
    );
    const denseGraph = visibleEdges.length > 2200 || visibleNodes.length > 700;
    const useDomOnlyInteractions =
      visibleNodes.length >= DOM_ONLY_INTERACTION_NODE_THRESHOLD;
    const useAllEdgesLayer =
      visibleNodes.length >= ALL_EDGES_LAYER_NODE_THRESHOLD ||
      visibleEdges.length > ALL_EDGES_LAYER_EDGE_THRESHOLD;
    const edgeColor = denseGraph
      ? resolvedTheme === 'dark'
        ? GRAPH_EDGE_COLORS.dense
        : GRAPH_EDGE_COLORS.denseLight
      : GRAPH_EDGE_COLORS.regular;
    const edgeSize = denseGraph ? (resolvedTheme === 'light' ? 0.34 : 0.28) : 0.42;
    const labelDensity = visibleNodes.length > 900 ? 0.016 : visibleNodes.length > 450 ? 0.022 : 0.045;
    // At ultra-dense node counts labels are disabled entirely below, so
    // `selectProminentGraphLabelIds` — which does an O(N log N) full
    // sort on the node array — is pointless work. Skip it on ultra-dense
    // graphs to keep initial build cost bounded.
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
        x: reusedPositions.get(node.id)?.x ?? 0,
        y: reusedPositions.get(node.id)?.y ?? 0,
        size,
        color,
        nodeType: node.type,
        forceLabel: showLabel,
      });
    }

    // Edge-render LOD: build the dense sample once, then hide/show the extra
    // edge ids through the reducer. This makes the toolbar density toggle a
    // partial repaint instead of changing the graph's edge cardinality, which
    // would force Sigma to run a full `process()` and hitch Firefox.
    const edgeSet = new Set<string>();
    const edgeLodExtraEdgeIds = new Set<string>();
    const connectedEndpointIds = new Set<string>();
    const endpointDegreeCounts = new Map<string, number>();
    for (const edge of visibleEdges) {
      connectedEndpointIds.add(edge.sourceId);
      connectedEndpointIds.add(edge.targetId);
      endpointDegreeCounts.set(edge.sourceId, (endpointDegreeCounts.get(edge.sourceId) ?? 0) + 1);
      endpointDegreeCounts.set(edge.targetId, (endpointDegreeCounts.get(edge.targetId) ?? 0) + 1);
    }
    const endpointBackboneBudget = Math.ceil(
      connectedEndpointIds.size * BASELINE_EDGE_ENDPOINT_COVERAGE_RATIO,
    );
    const defaultEdgeBudget = Math.min(
      visibleEdges.length,
      GRAPH_EDGE_DENSE_RENDER_CAP,
      Math.max(GRAPH_EDGE_RENDER_CAP, endpointBackboneBudget),
    );
    const endpointCoverageBudget = Math.min(
      defaultEdgeBudget,
      Math.ceil(defaultEdgeBudget * BASELINE_EDGE_ENDPOINT_COVERAGE_RATIO),
    );
    const localDetailBudget = Math.min(
      defaultEdgeBudget,
      Math.ceil(defaultEdgeBudget * BASELINE_EDGE_LOCAL_DETAIL_RATIO),
    );
    const denseEdgeBudget = Math.min(visibleEdges.length, GRAPH_EDGE_DENSE_RENDER_CAP);
    const defaultEdgeHashStride =
      visibleEdges.length > defaultEdgeBudget && defaultEdgeBudget > 0
        ? Math.ceil(visibleEdges.length / defaultEdgeBudget)
        : 1;
    const denseEdgeHashStride =
      visibleEdges.length > denseEdgeBudget && denseEdgeBudget > 0
        ? Math.ceil(visibleEdges.length / denseEdgeBudget)
        : 1;
    let baseSampledEdgeCount = 0;
    const endpointSampleCounts = new Map<string, number>();
    const uncoveredEndpointIds = new Set(connectedEndpointIds);
    const needsLocalDetail = (nodeId: string): boolean => {
      const degree = endpointDegreeCounts.get(nodeId) ?? 0;
      if (degree < 4) return false;
      return (endpointSampleCounts.get(nodeId) ?? 0) < baselineEdgeSampleQuota(degree);
    };
    const addSampledEdge = (edge: EdgeData, baseSample: boolean): boolean => {
      if (!graph.hasNode(edge.sourceId) || !graph.hasNode(edge.targetId)) return false;
      const key = edgePairKey(edge.sourceId, edge.targetId);
      if (edgeSet.has(key)) return false;
      edgeSet.add(key);
      try {
        const edgeId = graph.addEdge(edge.sourceId, edge.targetId, {
          label: edge.label || '',
          size: edgeSize,
          color: edgeColor,
          type: defaultEdgeType,
        });
        if (baseSample) {
          baseSampledEdgeCount += 1;
          endpointSampleCounts.set(edge.sourceId, (endpointSampleCounts.get(edge.sourceId) ?? 0) + 1);
          endpointSampleCounts.set(edge.targetId, (endpointSampleCounts.get(edge.targetId) ?? 0) + 1);
          uncoveredEndpointIds.delete(edge.sourceId);
          uncoveredEndpointIds.delete(edge.targetId);
        } else {
          edgeLodExtraEdgeIds.add(edgeId);
        }
        return true;
      } catch {
        return false;
      }
    };

    // Coverage-first LOD with local detail: a hash/stride sample can leave
    // ordinary nodes with no visible incident edge, while a pure endpoint
    // coverage pass gives hubs one or two lines despite hundreds of neighbors.
    // Build a visible backbone first, then spend most of the remaining budget
    // on degree-aware local context before the final deterministic fill.
    for (const edge of visibleEdges) {
      if (baseSampledEdgeCount >= endpointCoverageBudget || uncoveredEndpointIds.size === 0) break;
      if (!uncoveredEndpointIds.has(edge.sourceId) && !uncoveredEndpointIds.has(edge.targetId)) {
        continue;
      }
      addSampledEdge(edge, true);
    }
    for (const edge of visibleEdges) {
      if (baseSampledEdgeCount >= localDetailBudget) break;
      if (!needsLocalDetail(edge.sourceId) && !needsLocalDetail(edge.targetId)) {
        continue;
      }
      addSampledEdge(edge, true);
    }
    for (const edge of visibleEdges) {
      if (baseSampledEdgeCount >= defaultEdgeBudget) break;
      if (edgeSampleHash(edge) % defaultEdgeHashStride === 0) {
        addSampledEdge(edge, true);
      }
    }
    for (const edge of visibleEdges) {
      if (baseSampledEdgeCount >= defaultEdgeBudget) break;
      addSampledEdge(edge, true);
    }
    for (const edge of visibleEdges) {
      if (edgeSet.size >= denseEdgeBudget) break;
      if (edgeSampleHash(edge) % denseEdgeHashStride === 0) {
        addSampledEdge(edge, false);
      }
    }
    for (const edge of visibleEdges) {
      if (edgeSet.size >= denseEdgeBudget) break;
      addSampledEdge(edge, false);
    }
    edgeLodExtraEdgeIdsRef.current = edgeLodExtraEdgeIds;
    renderedEdgeIdsRef.current = graph.edges();

    // Slim payload describing this topology. Built ONCE here and stashed
    // in `workerPayloadRef` so the layout-switch effect can re-run the
    // worker without ever cloning the live Graphology instance — the
    // single most expensive op on the old switch path (~770 ms at 100k
    // nodes). The worker reads `{id, nodeType, size, label}` per node and
    // `{sourceId, targetId}` per edge; positions come back as a
    // transferable Float32Array (zero-copy).
    const workerNodes = visibleNodes.map((node) => ({
      id: node.id,
      nodeType: node.type,
      size: (graph.getNodeAttribute(node.id, 'size') as number | undefined) ?? 1,
      label: node.label,
    }));
    const workerEdges = visibleEdges.map((edge) => ({
      sourceId: edge.sourceId,
      targetId: edge.targetId,
    }));
    const workerPayload = { nodes: workerNodes, edges: workerEdges };
    workerPayloadRef.current = workerPayload;

    // Compute the INITIAL layout either synchronously or off-main-thread.
    // The worker path avoids a second main-thread Graphology build and
    // keeps the expensive force/component math off the critical frame
    // path. For graphs below `GRAPH_WORKER_NODE_THRESHOLD` the sync
    // codepath wins because the postMessage round-trip is pure overhead.
    const useWorker = visibleNodes.length >= GRAPH_WORKER_NODE_THRESHOLD;
    const layoutComputation: Promise<void> = reuseLayout
      ? Promise.resolve()
      : useWorker
      ? (async () => {
          try {
            const result = await computeGraphLayoutOffThread({
              nodes: workerNodes,
              edges: workerEdges,
              layout,
              cacheKey: workerPayload,
            });
            if (buildToken.cancelled) return;
            // Bulk-apply via `updateEachNodeAttributes` (one traversal,
            // mutating x/y in place) instead of 2N individual
            // `setNodeAttribute` calls — the latter each emit a Graphology
            // event Sigma would otherwise react to. Sigma is not attached
            // yet here, but the bulk form is still markedly cheaper.
            const positionById = new Map<string, number>();
            for (let i = 0; i < workerNodes.length; i += 1) {
              positionById.set(workerNodes[i].id, i);
            }
            graph.updateEachNodeAttributes(
              (id, attr) => {
                const i = positionById.get(id);
                if (i != null) {
                  attr.x = result.positions[i * 2];
                  attr.y = result.positions[i * 2 + 1];
                }
                return attr;
              },
              { attributes: ['x', 'y'] },
            );
          } catch (error) {
            // Worker failed (bundler misconfig, OOM, whatever). Do not run
            // the dense layout on the main thread: a simple O(N) fallback is
            // enough to keep the graph visible without a multi-frame stall.
            if (buildToken.cancelled) return;
            console.warn('[graph] worker layout failed, using cheap fallback layout', error);
            applyCheapFallbackLayout(graph);
          }
        })()
      : Promise.resolve().then(() => {
          applyGraphLayout(graph, layout);
        });

    let sigmaInstance: Sigma | null = null;
    let denseBaseEdgeLayersHidden = false;
    let denseEdgeRestoreFrame: number | null = null;
    const restoreSigmaNodeLayers = () => {
      const sigmaForLayers = sigmaInstance;
      if (!sigmaForLayers) return;
      const canvases = sigmaForLayers.getCanvases();
      for (const layer of SIGMA_NODE_CANVAS_LAYERS) {
        const canvas = canvases[layer];
        if (canvas) canvas.style.visibility = '';
      }
    };
    const setAllEdgesLayerHidden = (hidden: boolean) => {
      const canvas = allEdgesCanvasRef.current;
      if (canvas) canvas.style.visibility = hidden ? 'hidden' : '';
    };
    const setDenseBaseEdgeLayersHidden = (hidden: boolean) => {
      restoreSigmaNodeLayers();
      if (denseBaseEdgeLayersHidden === hidden) return;
      denseBaseEdgeLayersHidden = hidden;
      const sigmaForLayers = sigmaInstance;
      if (!sigmaForLayers) return;
      const canvases = sigmaForLayers.getCanvases();
      for (const layer of ['edges', 'edgeLabels'] as const) {
        const canvas = canvases[layer];
        if (canvas) canvas.style.visibility = hidden ? 'hidden' : '';
      }
    };
    const restoreDenseBaseEdgeLayers = () => {
      setDenseBaseEdgeLayersHidden(useAllEdgesLayer && allEdgesLayerEnabledRef.current);
    };

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

    // `hideEdgesOnMove` is gated on the RENDERED (post-cap) edge count, not the
    // raw total. With the edge cap active a GPU renders the sampled edge set at
    // full frame rate even during a camera move, so hiding edges mid-move would
    // only add a visible repaint hitch when the gesture ends (Sigma repaints
    // every edge once on release). We therefore keep edges visible while moving
    // for capped graphs and only hide them when the rendered edge set is truly
    // huge, where the per-frame edge pass would otherwise blow the frame
    // budget.
    const renderedEdgeCount = Math.min(visibleEdges.length, GRAPH_EDGE_DENSE_RENDER_CAP);
    const hideEdgesWhileMoving = renderedEdgeCount > 120000;

    const sigma = new Sigma(graph, containerRef.current, {
      hideEdgesOnMove: hideEdgesWhileMoving,
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
      zoomDuration: 50,
      zoomingRatio: 1.2,
      allowInvalidContainer: true,
    });

    sigmaInstance = sigma;
    const camera = sigma.getCamera();
    lastTopologyRef.current = { nodes, edges };

    // Node dragging.
    //
    // Every `graph.setNodeAttribute` emits Graphology's
    // `nodeAttributesUpdated`, and Sigma's internal listener responds by
    // re-running `updateNode` across ALL nodes and — because x/y are
    // layout-impacting — reprocessing the spatial index (O(N), no
    // skipIndexation). A high-poll mouse fires `mousemovebody` many times
    // per frame, so the naive handler paid that O(N) cost dozens of times
    // per frame on a 25k-node graph → drag stutter. Two guards fix it:
    //   1. Coalesce x AND y into ONE `mergeNodeAttributes` call so a single
    //      pointer move emits one update event, not two.
    //   2. rAF-throttle: stash the latest target position and commit it at
    //      most once per frame, capping drag cost at the 60 fps budget no
    //      matter how fast the mouse reports.
    let draggedNode: string | null = null;
    let pendingDragPos: { x: number; y: number } | null = null;
    let dragFrame: number | null = null;
    let draggedIncidentEdges: string[] = [];
    const refreshDraggedNode = (includeIncidentEdges: boolean) => {
      if (!draggedNode) return;
      const sigmaForDrag = sigmaRef.current;
      if (!sigmaForDrag) return;
      try {
        sigmaForDrag.refresh({
          partialGraph: {
            nodes: [draggedNode],
            edges: includeIncidentEdges ? draggedIncidentEdges : [],
          },
          skipIndexation: true,
        });
      } catch {
        sigmaForDrag.refresh({ schedule: true, skipIndexation: true });
      }
    };
    const refreshDenseIncidentEdgesThenRestore = (node: string, incidentEdges: string[]) => {
      const sigmaForDrag = sigmaRef.current;
      if (!sigmaForDrag || sigmaForDrag !== sigma || graphRef.current !== graph || !graph.hasNode(node)) {
        restoreDenseBaseEdgeLayers();
        return;
      }

      const edgesToRefresh = incidentEdges.filter((edge) => graph.hasEdge(edge));
      if (edgesToRefresh.length === 0) {
        sigmaForDrag.refresh({ partialGraph: { nodes: [node], edges: [] }, schedule: true, skipIndexation: true });
        restoreDenseBaseEdgeLayers();
        return;
      }

      let offset = 0;
      const chunkSize = 180;
      const refreshNextChunk = () => {
        denseEdgeRestoreFrame = null;
        if (buildToken.cancelled || sigmaRef.current !== sigma || graphRef.current !== graph || !graph.hasNode(node)) {
          restoreDenseBaseEdgeLayers();
          return;
        }

        const includeNode = offset === 0;
        const edgeChunk = edgesToRefresh.slice(offset, offset + chunkSize);
        offset += chunkSize;
        try {
          sigma.refresh({
            partialGraph: { nodes: includeNode ? [node] : [], edges: edgeChunk },
            schedule: true,
            skipIndexation: true,
          });
        } catch {
          sigma.refresh({ schedule: true, skipIndexation: true });
          restoreDenseBaseEdgeLayers();
          return;
        }

        if (offset < edgesToRefresh.length) {
          denseEdgeRestoreFrame = requestAnimationFrame(refreshNextChunk);
        } else {
          restoreDenseBaseEdgeLayers();
        }
      };

      denseEdgeRestoreFrame = requestAnimationFrame(refreshNextChunk);
    };
    const cacheDenseDragOverlayTargets = (node: string) => {
      const targets: Array<{ x: number; y: number }> = [];
      const hidden = hiddenIdsRef.current;
      const neighbors = neighborIndexRef.current.get(node) ?? new Set<string>();
      for (const neighbor of neighbors) {
        if (targets.length >= DRAG_NEIGHBORHOOD_OVERLAY_EDGE_LIMIT) break;
        if (hidden?.has(neighbor) || !graph.hasNode(neighbor)) continue;
        const targetX = graph.getNodeAttribute(neighbor, 'x') as number | undefined;
        const targetY = graph.getNodeAttribute(neighbor, 'y') as number | undefined;
        if (targetX == null || targetY == null) continue;
        targets.push(sigma.graphToViewport({ x: targetX, y: targetY }));
      }
      dragOverlayTargetsRef.current = targets;
    };
    const moveDragPreview = (clientX: number, clientY: number) => {
      const preview = dragPreviewRef.current;
      if (!preview) return;
      preview.style.transform = `translate3d(${clientX + 12}px, ${clientY + 12}px, 0)`;
    };
    const showDragPreview = (node: string, clientX: number, clientY: number) => {
      const preview = dragPreviewRef.current;
      if (!preview) return;
      preview.dataset.dragNodeId = node;
      preview.hidden = false;
      preview.style.visibility = 'visible';
      if (dragPreviewDotRef.current) {
        dragPreviewDotRef.current.style.backgroundColor =
          (graph.getNodeAttribute(node, 'color') as string | undefined) ?? GRAPH_NODE_COLORS.entity;
      }
      if (dragPreviewLabelRef.current) {
        dragPreviewLabelRef.current.textContent =
          labelByNodeId.get(node) ??
          (graph.getNodeAttribute(node, 'originalLabel') as string | undefined) ??
          node;
      }
      moveDragPreview(clientX, clientY);
    };
    const mergeNodePositionWithoutSigmaListener = (node: string, position: { x: number; y: number }) => {
      const saved = graph.listeners('nodeAttributesUpdated');
      graph.removeAllListeners('nodeAttributesUpdated');
      try {
        graph.mergeNodeAttributes(node, { x: position.x, y: position.y });
      } finally {
        for (const listener of saved) graph.on('nodeAttributesUpdated', listener);
      }
    };
    const flushDragPosition = () => {
      dragFrame = null;
      // `buildToken.cancelled` flips when the effect is torn down (topology
      // / layout change, unmount); skip so a queued frame never mutates a
      // graph whose Sigma instance was already killed.
      if (buildToken.cancelled || !draggedNode || !pendingDragPos) return;
      if (useDomOnlyInteractions) {
        // Dense graphs keep incident edges in the lightweight screen-space
        // overlay, but the node itself still has to move with the cursor.
        // Updating only one node per frame avoids the stale fixed dot without
        // paying the heavy incident-edge repaint path.
        mergeNodePositionWithoutSigmaListener(draggedNode, pendingDragPos);
        updateAllEdgesLayerNodePosition(draggedNode, pendingDragPos);
        refreshDraggedNode(false);
        drawNeighborhoodOverlayNow();
        pendingDragPos = null;
      } else {
        // Small graphs can afford to mutate the graph and repaint the dragged
        // node + incident edges each frame; the Sigma listener stays suppressed
        // so x/y updates do not trigger a full spatial reindex mid-drag.
        mergeNodePositionWithoutSigmaListener(draggedNode, pendingDragPos);
        refreshDraggedNode(true);
        pendingDragPos = null;
      }
    };

    sigma.on('downNode', ({ node }) => {
      draggedNode = node;
      const incidentEdges = graph.edges(node);
      draggedIncidentEdges = incidentEdges;
      dragStateRef.current = { dragging: true, node };
      pendingHoverRef.current = null;
      if (hoverTimerRef.current != null) {
        clearTimeout(hoverTimerRef.current);
        hoverTimerRef.current = null;
      }
      setHoveredId(null);
      setTooltip(null);
      setTooltipPos(null);
      if (useDomOnlyInteractions) {
        if (denseEdgeRestoreFrame != null) {
          cancelAnimationFrame(denseEdgeRestoreFrame);
          denseEdgeRestoreFrame = null;
        }
        setDenseBaseEdgeLayersHidden(true);
        if (allEdgesLayerStateRef.current?.kind !== 'indexed') {
          setAllEdgesLayerHidden(true);
        }
        cacheDenseDragOverlayTargets(node);
        neighborhoodOverlayFocusRef.current = { nodeId: node, mode: 'drag' };
        scheduleNeighborhoodOverlayDraw();
        const x = graph.getNodeAttribute(node, 'x') as number | undefined;
        const y = graph.getNodeAttribute(node, 'y') as number | undefined;
        const viewport =
          x == null || y == null ? { x: 0, y: 0 } : sigma.graphToViewport({ x, y });
        dragOverlaySourceRef.current = viewport;
        const rect = containerRef.current?.getBoundingClientRect();
        showDragPreview(node, viewport.x + (rect?.left ?? 0), viewport.y + (rect?.top ?? 0));
      } else {
        graph.setNodeAttribute(node, 'highlighted', true);
      }
      camera.disable();
    });

    sigma.getMouseCaptor().on('mousemovebody', (e: SigmaPointerCaptorEvent) => {
      if (!draggedNode) return;
      pendingDragPos = sigma.viewportToGraph(e);
      if (useDomOnlyInteractions) {
        moveDragPreview(e.original.clientX, e.original.clientY);
        const rect = containerRef.current?.getBoundingClientRect();
        dragOverlaySourceRef.current = {
          x: e.original.clientX - (rect?.left ?? 0),
          y: e.original.clientY - (rect?.top ?? 0),
        };
      }
      if (dragFrame == null) {
        dragFrame = requestAnimationFrame(flushDragPosition);
      }
      e.preventSigmaDefault();
      e.original.preventDefault();
      e.original.stopPropagation();
    });

    sigma.getMouseCaptor().on('mouseup', () => {
      if (draggedNode) {
        const releasedNode = draggedNode;
        const releasedIncidentEdges = draggedIncidentEdges;
        // Commit the final pointer position synchronously so the node
        // lands exactly where the cursor was released (a pending rAF would
        // otherwise drop the last sub-frame move).
        if (dragFrame != null) {
          cancelAnimationFrame(dragFrame);
          dragFrame = null;
        }
        if (pendingDragPos) {
          if (useDomOnlyInteractions) {
            mergeNodePositionWithoutSigmaListener(draggedNode, pendingDragPos);
            updateAllEdgesLayerNodePosition(draggedNode, pendingDragPos);
          } else {
            graph.mergeNodeAttributes(draggedNode, { x: pendingDragPos.x, y: pendingDragPos.y });
          }
          pendingDragPos = null;
        }
        if (useDomOnlyInteractions) {
          refreshDenseIncidentEdgesThenRestore(releasedNode, releasedIncidentEdges);
          if (allEdgesLayerStateRef.current?.kind === 'indexed') {
            scheduleAllEdgesLayerDraw();
          } else {
            rebuildAllEdgesLayer(graph, visibleEdges, useAllEdgesLayer);
          }
          setAllEdgesLayerHidden(false);
          hideDragPreview();
          dragOverlayTargetsRef.current = null;
          dragOverlaySourceRef.current = null;
          const selectedNode = selectedIdRef.current;
          neighborhoodOverlayFocusRef.current =
            selectedNode && graph.hasNode(selectedNode) ? { nodeId: selectedNode, mode: 'selected' } : null;
          scheduleNeighborhoodOverlayDraw();
        } else {
          graph.removeNodeAttribute(draggedNode, 'highlighted');
        }
        camera.enable();
        draggedNode = null;
        draggedIncidentEdges = [];
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
      if (dragStateRef.current.dragging) return;
      scheduleHoverUpdate(node);
      if (containerRef.current) containerRef.current.style.cursor = 'pointer';
      const neighborSet = neighborIndex.get(node);
      const neighborLabels: string[] = [];
      let neighborCount = 0;
      if (neighborSet) {
        neighborCount = neighborSet.size;
        let sampled = 0;
        for (const id of neighborSet) {
          neighborLabels.push(labelByNodeId.get(id) ?? id);
          sampled += 1;
          if (sampled >= 12) break;
        }
      }
      const label =
        labelByNodeId.get(node) ??
        (graph.getNodeAttribute(node, 'originalLabel') as string | undefined) ??
        node;
      setTooltip({
        nodeId: node,
        label,
        neighborLabels,
        neighborCount,
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
      if (dragStateRef.current.dragging) return;
      scheduleHoverUpdate(null);
      if (containerRef.current) containerRef.current.style.cursor = 'default';
      setTooltip(null);
      setTooltipPos(null);
    });
    // Reposition the card on camera move so it stays glued to the node
    // when the user pans/zooms with the hover still active.
    camera.on('updated', () => {
      if (useAllEdgesLayer && !suspendAllEdgesLayerAutoDrawRef.current) {
        scheduleAllEdgesLayerDraw();
      }
      if (useDomOnlyInteractions && neighborhoodOverlayFocusRef.current) {
        scheduleNeighborhoodOverlayDraw();
      }
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
    sigma.on('afterRender', () => {
      if (useAllEdgesLayer && !suspendAllEdgesLayerAutoDrawRef.current) {
        scheduleAllEdgesLayerDraw();
      }
      if (useDomOnlyInteractions && neighborhoodOverlayFocusRef.current) {
        scheduleNeighborhoodOverlayDraw();
      }
    });

    sigma.on('clickNode', ({ node }) => {
      if (!dragStateRef.current.dragging) {
        if (useDomOnlyInteractions) {
          neighborhoodOverlayFocusRef.current = { nodeId: node, mode: 'selected' };
          scheduleNeighborhoodOverlayDraw();
        }
        onSelectRef.current(node);
      }
    });
    sigma.on('clickStage', () => {
      setHoveredId(null);
      if (!dragStateRef.current.dragging) {
        if (useDomOnlyInteractions) {
          neighborhoodOverlayFocusRef.current = null;
          scheduleNeighborhoodOverlayDraw();
        }
        onSelectRef.current(null);
      }
    });

    sigmaRef.current = sigma;
    // Fresh Sigma instance owns a fresh program index. Any node/edge ids
    // tracked from a previous topology are now invalid and must never be
    // handed to `partialGraph` + `skipIndexation` (Sigma throws
    // "can't be repaint" for an id it has no program slot for). Clear the
    // affected-set trackers so the next reducer run starts from empty.
    affectedNodeIdsRef.current = new Set();
    affectedEdgeIdsRef.current = new Set();
    hiddenEdgeIdsRef.current = null;
    setGraphInstanceVersion((version) => version + 1);
    rebuildAllEdgesLayer(graph, visibleEdges, useAllEdgesLayer);
    restoreDenseBaseEdgeLayers();
    onFitViewReadyRef.current?.(() => {
      void sigmaRef.current?.getCamera().animatedReset({ duration: 280 });
    });
    if (reusedCameraState) {
      sigma.getCamera().setState(reusedCameraState);
    } else {
      requestAnimationFrame(() => {
        void sigma.getCamera().animatedReset({ duration: 180 });
      });
    }
    });

    return () => {
      // Abort any in-flight worker layout before the cleanup runs so
      // the `.then` body short-circuits before it ever touches Sigma.
      buildToken.cancelled = true;
      stopLayoutAnimation();
      // Invalidate any in-flight layout-switch worker result (it guards on
      // this token) and clear the recomputing affordance so a topology /
      // library change never leaves the spinner stuck on.
      layoutSwitchTokenRef.current += 1;
      forceClearAllEdgesLayerSuspension();
      setLayoutRecomputing(false);
      if (hoverTimerRef.current != null) {
        clearTimeout(hoverTimerRef.current);
        hoverTimerRef.current = null;
      }
      pendingHoverRef.current = null;
      setHoveredId(null);
      setTooltip(null);
      neighborhoodOverlayFocusRef.current = null;
      dragOverlayTargetsRef.current = null;
      dragOverlaySourceRef.current = null;
      clearNeighborhoodOverlay();
      clearAllEdgesLayer();
      hideDragPreview();
      if (denseEdgeRestoreFrame != null) {
        cancelAnimationFrame(denseEdgeRestoreFrame);
        denseEdgeRestoreFrame = null;
      }
      setDenseBaseEdgeLayersHidden(false);
      if (sigmaInstance) {
        lastCameraStateRef.current = sigmaInstance.getCamera().getState();
        sigmaInstance.kill();
      }
      renderedEdgeIdsRef.current = [];
      sigmaRef.current = null;
    };
    // `layout` is intentionally NOT a dependency. This effect tears down and
    // rebuilds the entire Graphology graph + Sigma instance, which is only
    // warranted when the TOPOLOGY changes. A layout/mode switch must NOT
    // rebuild — it is handled by the dedicated layout-switch effect below,
    // which just re-applies node positions to the existing instance. Including
    // `layout` here made every mode switch pay a full graph rebuild on top of
    // the position apply. The effect closure still reads the current `layout`
    // when it DOES run (React recreates the closure per render), so a topology
    // change still builds with the active layout.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    clearAllEdgesLayer,
    clearNeighborhoodOverlay,
    drawNeighborhoodOverlayNow,
    edges,
    forceClearAllEdgesLayerSuspension,
    labelByNodeId,
    neighborIndex,
    nodes,
    rebuildAllEdgesLayer,
    resolvedTheme,
    scheduleAllEdgesLayerDraw,
    scheduleNeighborhoodOverlayDraw,
    updateAllEdgesLayerNodePosition,
  ]);

  useEffect(() => {
    const sigma = sigmaRef.current;
    const graph = graphRef.current;
    if (!sigma || !graph || nodes.length === 0) return;
    if (layoutRef.current === layout) return;

    stopLayoutAnimation();
    const previousLayout = layoutRef.current;
    layoutRef.current = layout;

    const reduceMotion =
      typeof window !== 'undefined' &&
      typeof window.matchMedia === 'function' &&
      window.matchMedia('(prefers-reduced-motion: reduce)').matches;

    const order = graph.order;
    const useAllEdgesLayer =
      nodes.length >= ALL_EDGES_LAYER_NODE_THRESHOLD ||
      edges.length > ALL_EDGES_LAYER_EDGE_THRESHOLD;

    // Snap-in the new positions and repaint in a single bounded pass.
    //   * `updateEachNodeAttributes` mutates x/y in ONE Graphology
    //     traversal (vs 2N individual `setNodeAttribute` calls, each of
    //     which fires an event Sigma reacts to).
    //   * `refresh({ skipIndexation: true })` reuses every node's program
    //     slot — only x/y changed, no node was added/removed and draw
    //     order is unaffected, so the full O(N) spatial reindex Sigma
    //     would otherwise run is skipped. Camera reset reframes the new
    //     layout in one animated paint. Hit detection is still driven by
    //     the rendered picking buffers after repaint; forcing Sigma's
    //     full `process()` here was the remaining 200ms+ mode-switch
    //     hitch on dense graphs.
    const applyPositionsInstant = (positionFor: (id: string) => { x: number; y: number } | null) => {
      // Mutating x/y for every node and getting Sigma to repaint the new layout
      // is a single, unavoidable O(N) reprocess — but the naive path pays it
      // TWICE. `updateEachNodeAttributes` fires Graphology's
      // `eachNodeAttributesUpdated`, and Sigma's listener for it runs a full
      // `updateNode` sweep AND schedules a second full reprocess
      // (`skipIndexation: false`, x/y being layout-impacting; grounded in
      // sigma@3.0.3 `eachNodeAttributesUpdatedGraphUpdate`). That double pass
      // is the layout-mode switch hitch this path avoids.
      //
      // So we suppress that listener (via Graphology's public EventEmitter API)
      // while we write positions — no Sigma reaction — then restore it and run
      // ONE explicit `sigma.refresh()`, collapsing the work to a single
      // process() pass. The camera reframe stays explicit.
      const EVENT = 'eachNodeAttributesUpdated';
      const saved = graph.listeners(EVENT);
      graph.removeAllListeners(EVENT);
      try {
        graph.updateEachNodeAttributes(
          (id, attr) => {
            const pos = positionFor(id);
            if (pos) {
              attr.x = pos.x;
              attr.y = pos.y;
            }
            return attr;
          },
          { attributes: ['x', 'y'] },
        );
      } finally {
        for (const listener of saved) graph.on(EVENT, listener);
      }
      sigma.refresh({ skipIndexation: true });
      syncOrRebuildAllEdgesLayerPositions(graph, edges, useAllEdgesLayer);
      void sigma.getCamera().animatedReset({ duration: 200 });
    };

    const applyPositionsChunked = async (
      layoutNodes: Array<{ id: string }>,
      positions: ArrayLike<number>,
      token: number,
    ): Promise<boolean> => {
      const nodeEvent = 'nodeAttributesUpdated';
      const chunkSize = order >= 50000 ? 500 : 5000;
      const refreshRenderedEdgesChunked = async (): Promise<boolean> => {
        const cachedRenderedEdges = renderedEdgeIdsRef.current;
        const renderedEdges = cachedRenderedEdges.length > 0 ? cachedRenderedEdges : graph.edges();
        const edgeChunkSize = order >= 50000 ? 500 : 4000;
        for (let offset = 0; offset < renderedEdges.length; offset += edgeChunkSize) {
          if (layoutSwitchTokenRef.current !== token) return false;
          if (graphRef.current !== graph || sigmaRef.current !== sigma) return false;
          const edgeChunk = renderedEdges.slice(offset, offset + edgeChunkSize);
          sigma.refresh({ partialGraph: { nodes: [], edges: edgeChunk }, schedule: true, skipIndexation: true });
          await new Promise<void>((resolve) => {
            requestAnimationFrame(() => resolve());
          });
        }
        return true;
      };

      for (let offset = 0; offset < layoutNodes.length; offset += chunkSize) {
        if (layoutSwitchTokenRef.current !== token) return false;
        if (graphRef.current !== graph || sigmaRef.current !== sigma) return false;

        const chunkIds: string[] = [];
        const savedNodeListeners = graph.listeners(nodeEvent);
        graph.removeAllListeners(nodeEvent);
        try {
          const limit = Math.min(layoutNodes.length, offset + chunkSize);
          for (let sourceIndex = offset; sourceIndex < limit; sourceIndex += 1) {
            const id = layoutNodes[sourceIndex].id;
            chunkIds.push(id);
            graph.mergeNodeAttributes(id, {
              x: positions[sourceIndex * 2],
              y: positions[sourceIndex * 2 + 1],
            });
          }
        } finally {
          for (const listener of savedNodeListeners) graph.on(nodeEvent, listener);
        }

        sigma.refresh({ partialGraph: { nodes: chunkIds, edges: [] }, schedule: true, skipIndexation: true });

        await new Promise<void>((resolve) => {
          requestAnimationFrame(() => resolve());
        });
      }

      if (layoutSwitchTokenRef.current !== token) return false;
      if (graphRef.current !== graph || sigmaRef.current !== sigma) return false;
      // Node chunks were repainted above, but Sigma's sampled edge program
      // still holds endpoints from the previous layout. Dense graphs draw
      // canonical links through the indexed full-edge overlay, with Sigma's
      // sampled edge canvas hidden, so refreshing the sampled layer would only
      // add a long queued repaint. `useAllEdgesLayer` alone is only intent:
      // if WebGL/indexed setup failed, Sigma's sampled edge layer remains the
      // visible fallback and must be refreshed against the new node positions.
      const hasActiveIndexedAllEdgesLayer =
        useAllEdgesLayer &&
        allEdgesLayerEnabledRef.current &&
        allEdgesLayerStateRef.current?.kind === 'indexed';
      if (!hasActiveIndexedAllEdgesLayer && !(await refreshRenderedEdgesChunked())) return false;
      const isCurrent = () => layoutSwitchTokenRef.current === token && graphRef.current === graph && sigmaRef.current === sigma;
      const indexedLayerState = await writeAllEdgesLayerPositionsFromLayoutChunked(
        layoutNodes,
        positions,
        useAllEdgesLayer,
        isCurrent,
      );
      if (indexedLayerState) {
        const uploaded = await uploadAllEdgesLayerPositionTextureChunked(indexedLayerState, isCurrent);
        if (!uploaded) return false;
      } else {
        rebuildAllEdgesLayer(graph, edges, useAllEdgesLayer);
      }
      scheduleNeighborhoodOverlayDraw();
      if (order < 50000) {
        void sigma.getCamera().animatedReset({ duration: 200 });
      }
      await waitForAnimationFrame();
      if (layoutSwitchTokenRef.current !== token) return false;
      if (graphRef.current !== graph || sigmaRef.current !== sigma) return false;
      await waitForAnimationFrame();
      if (layoutSwitchTokenRef.current !== token) return false;
      if (graphRef.current !== graph || sigmaRef.current !== sigma) return false;
      return true;
    };

    // ULTRA-DENSE (≥ INSTANT_LAYOUT_NODE_THRESHOLD, e.g. 100k nodes).
    // Per-frame interpolation is pointless (the eye cannot track 100k
    // dots drifting) AND computing the target layout on the main thread
    // is a ~1 s stall — the clone alone was ~770 ms at 100k. Route the
    // whole layout computation to the worker using the cached slim
    // payload (no clone, no second main-thread Graphology build), show a
    // "recomputing" affordance, then apply the result in frame-sized
    // chunks so Firefox never gets one large repaint/upload burst.
    const payload = workerPayloadRef.current;
    if (order >= INSTANT_LAYOUT_NODE_THRESHOLD && payload) {
      const token = layoutSwitchTokenRef.current + 1;
      layoutSwitchTokenRef.current = token;
      if (useAllEdgesLayer) {
        suspendAllEdgesLayerDraws(token);
      }
      setLayoutRecomputing(true);
      void (async () => {
        let applied = false;
        try {
          const result = await computeGraphLayoutOffThread({
            nodes: payload.nodes,
            edges: payload.edges,
            layout,
            cacheKey: payload,
          });
          // Discard if a newer switch superseded this one, or the graph
          // was torn down (topology change / unmount) while computing.
          if (layoutSwitchTokenRef.current !== token) return;
          if (graphRef.current !== graph || sigmaRef.current !== sigma) return;
          applied = await applyPositionsChunked(payload.nodes, result.positions, token);
          if (applied && useAllEdgesLayer) {
            resumeAllEdgesLayerDraws(token, true);
          }
        } catch (error) {
          if (layoutSwitchTokenRef.current !== token) return;
          if (graphRef.current !== graph || sigmaRef.current !== sigma) return;
          // Keep the previous layout rather than reintroducing the dense
          // synchronous layout stall on the UI thread.
          console.warn('[graph] layout switch failed, keeping previous layout', error);
          layoutRef.current = previousLayout;
        } finally {
          if (layoutSwitchTokenRef.current === token) {
            if (useAllEdgesLayer && !applied) {
              resumeAllEdgesLayerDraws(token, true);
            }
            setLayoutRecomputing(false);
          }
        }
      })();
      return () => {
        stopLayoutAnimation();
      };
    }

    // MID-DENSITY instant path (no animation but cheap to compute on the
    // main thread). Compute the target layout directly on the live graph
    // — there is no clone. The brief sync layout pass is well under a
    // frame at these node counts, and we never interpolate.
    if (reduceMotion || order === 0 || order >= INSTANT_LAYOUT_NODE_THRESHOLD) {
      applyGraphLayout(graph, layout);
      sigma.refresh({ skipIndexation: true });
      syncOrRebuildAllEdgesLayerPositions(graph, edges, useAllEdgesLayer);
      scheduleNeighborhoodOverlayDraw();
      void sigma.getCamera().animatedReset({ duration: 140 });
      return () => {
        stopLayoutAnimation();
      };
    }

    // SMALL graph: keep the beautiful eased per-frame transition. Compute
    // the target positions WITHOUT cloning — snapshot the current x/y,
    // apply the target layout to the live graph to read the destination,
    // then restore the from-positions and animate between them. At these
    // node counts the double layout pass is negligible and the smooth
    // drift is the whole point of the small-graph experience.
    const transitionNodes: Array<{ node: string; fromX: number; fromY: number; toX: number; toY: number }> = [];
    graph.forEachNode((node, attr) => {
      transitionNodes.push({
        node,
        fromX: (attr.x as number) ?? 0,
        fromY: (attr.y as number) ?? 0,
        toX: 0,
        toY: 0,
      });
    });
    applyGraphLayout(graph, layout);
    for (const transition of transitionNodes) {
      transition.toX = (graph.getNodeAttribute(transition.node, 'x') as number) ?? 0;
      transition.toY = (graph.getNodeAttribute(transition.node, 'y') as number) ?? 0;
      // Restore the starting position so the first animated frame begins
      // from where the node currently is, not from its destination.
      graph.setNodeAttribute(transition.node, 'x', transition.fromX);
      graph.setNodeAttribute(transition.node, 'y', transition.fromY);
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

      sigma.refresh({ skipIndexation: true });

      if (progress < 1) {
        layoutAnimationFrameRef.current = requestAnimationFrame(renderFrame);
      } else {
        layoutAnimationFrameRef.current = null;
        syncOrRebuildAllEdgesLayerPositions(graph, edges, useAllEdgesLayer);
        void sigma.getCamera().animatedReset({ duration: 180 });
        scheduleNeighborhoodOverlayDraw();
      }
    };

    layoutAnimationFrameRef.current = requestAnimationFrame(renderFrame);

    return () => {
      stopLayoutAnimation();
    };
  }, [
    edges,
    layout,
    nodes,
    rebuildAllEdgesLayer,
    resumeAllEdgesLayerDraws,
    scheduleNeighborhoodOverlayDraw,
    syncOrRebuildAllEdgesLayerPositions,
    suspendAllEdgesLayerDraws,
    uploadAllEdgesLayerPositionTextureChunked,
    writeAllEdgesLayerPositionsFromLayoutChunked,
  ]);

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
    const useAllEdgesLayer =
      nodes.length >= ALL_EDGES_LAYER_NODE_THRESHOLD ||
      edges.length > ALL_EDGES_LAYER_EDGE_THRESHOLD;
    if (!hiddenIds || hiddenIds.size === 0) {
      hiddenEdgeIdsRef.current = null;
      rebuildAllEdgesLayer(graph, edges, useAllEdgesLayer);
      return;
    }
    const hidden = new Set<string>();
    graph.forEachEdge((edge, _attrs, source, target) => {
      if (hiddenIds.has(source) || hiddenIds.has(target)) {
        hidden.add(edge);
      }
    });
    hiddenEdgeIdsRef.current = hidden;
    rebuildAllEdgesLayer(graph, edges, useAllEdgesLayer);
  }, [hiddenIds, nodes, edges, graphInstanceVersion, rebuildAllEdgesLayer]);

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
    const filterHiddenEdgeIds = hiddenEdgeIdsRef.current ?? EMPTY_EDGE_SET;
    const lodHiddenEdgeIds = showDenseEdges ? EMPTY_EDGE_SET : edgeLodExtraEdgeIdsRef.current;
    const hasHiddenEdges = filterHiddenEdgeIds.size > 0 || lodHiddenEdgeIds.size > 0;
    const isEdgeHidden = (edge: string): boolean =>
      filterHiddenEdgeIds.has(edge) || lodHiddenEdgeIds.has(edge);
    const useDomOnlyInteractions =
      nodes.length >= DOM_ONLY_INTERACTION_NODE_THRESHOLD;

    // Accumulate the ids this run visually changes. Only these — plus the
    // ones the PREVIOUS run changed (so we can restore them to base style)
    // — get re-reduced + repainted via `partialGraph`. Everything else
    // keeps its already-rendered attributes untouched, turning the former
    // O(N) refresh into an O(affected) one.
    const nextAffectedNodes = new Set<string>();
    const nextAffectedEdges = new Set<string>();
    // `skipIndexation: true` reuses each element's existing program slot
    // and skips Sigma's whole-graph reprocess — correct for pure visual
    // attribute changes (size / color / label / hidden / highlighted).
    // EVERY branch keeps this true, so there is never a per-interaction
    // GPU reindex.
    //
    // Sigma treats `zIndex` as a layout-impacting field: a reducer-written
    // `zIndex` only re-sorts draw order inside `process()`, which runs only
    // when `skipIndexation` is false (grounded in sigma@3.0.3
    // `refresh()` → `needToProcess`/`zIndexOrdering`). The selection branch
    // used to rewrite `zIndex` graph-wide to layer the focused node +
    // neighbors above the faded rest, which forced a full O(N) reindex on
    // every click AND every deselect — the click freeze on a 100k-node /
    // ~300k-edge graph. We removed those `zIndex` writes: the selected node
    // already draws on the dedicated `highlightedNodes` top pass (via
    // `highlighted: true`), independent of the z-sort, and the size contrast
    // (focus 9 / neighbor 7 vs faded 2) plus near-invisible white/size-0.05
    // faded edges preserve the spotlight without a reindex. Now selection
    // and deselection are reducer-cache-only — no GPU reprocess, no freeze.
    const skipIndexation = true;

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
    if (!hiddenNodeSet && !hasHiddenEdges && useDomOnlyInteractions) {
      sigma.setSetting('nodeReducer', null);
      sigma.setSetting('edgeReducer', null);
      affectedNodeIdsRef.current = new Set();
      affectedEdgeIdsRef.current = new Set();
      return;
    }

    if (!useDomOnlyInteractions && selectedId && graph.hasNode(selectedId)) {
      // `graph.edges(node)` returns only the edges incident to
      // `selectedId` — O(degree) instead of O(M). The previous code
      // walked every edge every time the user clicked a node, which was
      // visibly janky.
      const connectedEdges = new Set<string>(graph.edges(selectedId));
      const neighbors = neighborIndex.get(selectedId) ?? new Set<string>();

      // Selection is O(degree), NOT O(graph). Earlier this branch faded the
      // ENTIRE graph into the partial refresh and restyled every element, which
      // is a multi-100ms main-thread block on EACH click and deselect even with
      // `skipIndexation: true` (Sigma still loops every id calling
      // `updateNode`/`updateEdge`; sigma `refresh()` partial loop). On a graph
      // this size that whole-graph dim is unaffordable per interaction.
      //
      // Instead we EMPHASISE the focus neighbourhood rather than dimming the
      // rest: the selected node + its neighbours pop (bigger, labelled, on
      // Sigma's `highlightedNodes` top pass) and their connecting edges
      // highlight. Everything else keeps its base style untouched, so the
      // affected set is just {focus ∪ neighbours} and {incident edges} —
      // O(degree). The reducers below still return base `data` for the rest,
      // so a later full `process()` (e.g. on camera-move release) renders the
      // whole graph consistently; we simply never force-repaint all of it on
      // a click.
      nextAffectedNodes.add(selectedId);
      for (const neighbor of neighbors) nextAffectedNodes.add(neighbor);
      for (const edge of connectedEdges) nextAffectedEdges.add(edge);

      sigma.setSetting('nodeReducer', (node: string, data: SigmaReducerData) => {
        if (hiddenNodeSet && hiddenNodeSet.has(node)) {
          return { ...data, hidden: true, label: '' };
        }
        if (node === selectedId) {
          return {
            ...data,
            size: Math.max((data.size ?? 0), 9),
            label: data.focusLabel ?? data.displayLabel ?? data.label,
            forceLabel: true,
            highlighted: true,
          };
        }
        if (neighbors.has(node)) {
          return {
            ...data,
            size: Math.max((data.size ?? 0), 7),
            label: data.focusLabel ?? data.displayLabel ?? data.label,
            forceLabel: true,
            highlighted: true,
          };
        }
        return data;
      });

      sigma.setSetting('edgeReducer', (edge: string, data: SigmaReducerData) => {
        if (isEdgeHidden(edge)) {
          return { ...data, hidden: true };
        }
        if (connectedEdges.has(edge)) {
          return {
            ...data,
            color: GRAPH_EDGE_COLORS.highlight,
            size: Math.max((data.size ?? 0), 1.2),
          };
        }
        return data;
      });
    } else if (!useDomOnlyInteractions && hoveredId && graph.hasNode(hoveredId)) {
      // The dwell-time gate (`HOVER_DWELL_MS`) ensures this branch only
      // runs when the user actually pauses on a node, not on every
      // mousemove. So we can afford a real `nodeReducer` here that bumps
      // both the hovered node and its neighbors with labels — the
      // ~120 ms refresh happens once per intentional hover, not 60 times
      // per second during a sweep.
      const neighbors = neighborIndex.get(hoveredId) ?? new Set<string>();
      // Hover only restyles the hovered node and its neighbors — an
      // O(degree) NODE set. This is the path that used to pay a full
      // O(N) `sigma.refresh()` and cause the on-stop freeze; now it
      // partial-refreshes just this neighborhood (∪ the previous one).
      // The hover `edgeReducer` is null (or hidden-only) so it never
      // restyles incident edges — no edges need repainting here.
      nextAffectedNodes.add(hoveredId);
      for (const neighbor of neighbors) nextAffectedNodes.add(neighbor);
      sigma.setSetting('nodeReducer', (node: string, data: SigmaReducerData) => {
        if (hiddenNodeSet && hiddenNodeSet.has(node)) {
          return { ...data, hidden: true, label: '' };
        }
        if (node === hoveredId) {
          return {
            ...data,
            size: Math.max((data.size ?? 0), 11),
            label: data.focusLabel ?? data.displayLabel ?? data.label,
            forceLabel: true,
            highlighted: true,
          };
        }
        if (neighbors.has(node)) {
          return {
            ...data,
            size: Math.max((data.size ?? 0), 8),
            label: data.focusLabel ?? data.displayLabel ?? data.label,
            forceLabel: true,
          };
        }
        return data;
      });
      if (hasHiddenEdges) {
        sigma.setSetting('edgeReducer', (edge: string, data: SigmaReducerData) => {
          if (isEdgeHidden(edge)) return { ...data, hidden: true };
          return data;
        });
      } else {
        sigma.setSetting('edgeReducer', null);
      }
    } else if (hiddenNodeSet || hasHiddenEdges) {
      // Pure filter mode: no selection, no hover, but filters are active.
      // Hide nodes/edges without touching anything else. Only the hidden
      // ids change from base style, so they are the affected set; the
      // union with the previous run's affected set restores any element
      // that just became visible again after a legend/search toggle.
      if (hiddenNodeSet) {
        for (const node of hiddenNodeSet) nextAffectedNodes.add(node);
      }
      for (const edge of filterHiddenEdgeIds) nextAffectedEdges.add(edge);
      for (const edge of lodHiddenEdgeIds) nextAffectedEdges.add(edge);
      sigma.setSetting(
        'nodeReducer',
        hiddenNodeSet
          ? (node: string, data: SigmaReducerData) => {
              if (hiddenNodeSet.has(node)) return { ...data, hidden: true, label: '' };
              return data;
            }
          : null,
      );
      sigma.setSetting('edgeReducer', (edge: string, data: SigmaReducerData) => {
        if (isEdgeHidden(edge)) return { ...data, hidden: true };
        return data;
      });
    } else {
      // Fully idle: clear reducers. Nothing new is affected; the union
      // with the previous affected set repaints whatever was highlighted
      // back to its base style.
      sigma.setSetting('nodeReducer', null);
      sigma.setSetting('edgeReducer', null);
    }

    // Partial refresh over {previously affected} ∪ {now affected}: Sigma
    // re-applies the reducers and repaints ONLY these ids, reusing every
    // other element's already-computed render data. `skipIndexation` is
    // true for EVERY branch now (no branch rewrites x/y or zIndex — the
    // selection branch dropped its zIndex writes in favor of the
    // `highlightedNodes` top pass), so Sigma always skips the whole-graph
    // `process()`/`zIndexOrdering` reprocess. Hover/filter/idle stay
    // O(affected); selection touches the full node/edge set but only as
    // cheap cache writes, never a GPU reindex — eliminating both the
    // ~120 ms hover-stop block and the click/deselect freeze on large
    // graphs. (This mirrors Sigma's own `eachNodeAttributesUpdated`
    // internal path, which likewise calls
    // `refresh({ partialGraph, skipIndexation })`.) The id list is filtered
    // through `graph.hasNode`/`hasEdge` so a stale id can never reach
    // `skipIndexation`'s repaint, which throws for an unindexed element.
    const refreshNodes: string[] = [];
    const seenNode = new Set<string>();
    for (const id of affectedNodeIdsRef.current) {
      if (graph.hasNode(id) && !seenNode.has(id)) {
        seenNode.add(id);
        refreshNodes.push(id);
      }
    }
    for (const id of nextAffectedNodes) {
      if (!seenNode.has(id) && graph.hasNode(id)) {
        seenNode.add(id);
        refreshNodes.push(id);
      }
    }
    const refreshEdges: string[] = [];
    const seenEdge = new Set<string>();
    for (const id of affectedEdgeIdsRef.current) {
      if (graph.hasEdge(id) && !seenEdge.has(id)) {
        seenEdge.add(id);
        refreshEdges.push(id);
      }
    }
    for (const id of nextAffectedEdges) {
      if (!seenEdge.has(id) && graph.hasEdge(id)) {
        seenEdge.add(id);
        refreshEdges.push(id);
      }
    }

    if (refreshNodes.length === 0 && refreshEdges.length > 2000) {
      let cancelled = false;
      const chunkSize = 100;
      void (async () => {
        for (let offset = 0; offset < refreshEdges.length; offset += chunkSize) {
          if (cancelled || graphRef.current !== graph || sigmaRef.current !== sigma) return;
          const edgeChunk = refreshEdges.slice(offset, offset + chunkSize);
          try {
            sigma.refresh({
              partialGraph: { nodes: [], edges: edgeChunk },
              schedule: true,
              skipIndexation,
            });
          } catch {
            sigma.refresh({ schedule: true });
            return;
          }
          await new Promise<void>((resolve) => {
            requestAnimationFrame(() => resolve());
          });
        }
      })();

      affectedNodeIdsRef.current = nextAffectedNodes;
      affectedEdgeIdsRef.current = nextAffectedEdges;
      return () => {
        cancelled = true;
      };
    }

    // `skipIndexation: true` repaints each id into its EXISTING program slot
    // (`edgeProgramIndex[id]` / `nodeProgramIndex[id]`) and throws
    // "can't be repaint" if that slot is missing — which happens when this
    // reducer effect runs against a freshly-rebuilt Sigma instance that has
    // not completed its first `process()` yet (the build effect and this
    // effect can interleave on a data/selection change). An uncaught throw
    // here propagates out of Sigma's render and loses the WebGL context,
    // blanking the canvas. So fall back to a full `sigma.refresh()` (which
    // re-indexes the whole graph and can never throw). Steady-state
    // interactions keep the fast partial path; only the rare pre-process
    // window pays one full refresh.
    try {
      sigma.refresh({
        partialGraph: { nodes: refreshNodes, edges: refreshEdges },
        skipIndexation,
      });
    } catch {
      sigma.refresh();
    }

    affectedNodeIdsRef.current = nextAffectedNodes;
    affectedEdgeIdsRef.current = nextAffectedEdges;
  }, [
    hoveredId,
    neighborIndex,
    selectedId,
    hiddenIds,
    showDenseEdges,
    graphInstanceVersion,
    nodes.length,
  ]);

  return (
    <div className="relative isolate h-full w-full">
      <canvas
        ref={allEdgesCanvasRef}
        aria-hidden="true"
        className="pointer-events-none absolute inset-0 z-0 h-full w-full"
      />
      <div ref={containerRef} className="relative z-10 h-full w-full" style={{ minHeight: '400px' }} />
      <canvas
        ref={neighborhoodCanvasRef}
        aria-hidden="true"
        className="pointer-events-none absolute inset-0 z-20 h-full w-full"
      />
      {layoutRecomputing && (
        <div
          role="status"
          aria-live="polite"
          className="pointer-events-none absolute left-1/2 top-4 z-40 -translate-x-1/2 inline-flex items-center gap-2 rounded-full border border-border/70 bg-popover/90 px-3 py-1.5 text-xs font-medium text-popover-foreground shadow-lg backdrop-blur-sm"
        >
          <Loader2 className="h-3.5 w-3.5 animate-spin text-primary/70" />
          {t('graph.recomputingLayout')}
        </div>
      )}
      {tooltip && tooltipPos && (
        <div
          ref={tooltipRef}
          data-node-id={tooltip.nodeId}
          className="fixed pointer-events-none z-50 max-w-xs rounded-md border border-border bg-popover/95 px-3 py-2 text-xs text-popover-foreground shadow-lg backdrop-blur-sm"
          style={{ left: tooltipPos.x + 12, top: tooltipPos.y + 12 }}
        >
          <div className="font-semibold text-sm leading-tight mb-1 truncate">{tooltip.label}</div>
          <div className="text-muted-foreground text-[11px] mb-1">
            {t('graph.edgeCount', { count: tooltip.neighborCount })}
          </div>
          {tooltip.neighborLabels.length > 0 && (
            <ul className="space-y-0.5 list-disc list-inside text-[11px] text-muted-foreground">
              {tooltip.neighborLabels.map((label, i) => (
                <li key={i} className="truncate">{label}</li>
              ))}
              {tooltip.neighborCount > tooltip.neighborLabels.length && (
                <li className="text-muted-foreground/70">
                  {t('common.moreCount', {
                    count: tooltip.neighborCount - tooltip.neighborLabels.length,
                  })}
                </li>
              )}
            </ul>
          )}
        </div>
      )}
      <div
        ref={dragPreviewRef}
        className="pointer-events-none fixed left-0 top-0 z-50 flex max-w-xs items-center gap-2 rounded-full border border-border/70 bg-popover/95 px-2.5 py-1.5 text-xs font-medium text-popover-foreground shadow-lg backdrop-blur-sm will-change-transform"
      >
        <span ref={dragPreviewDotRef} className="h-2.5 w-2.5 shrink-0 rounded-full" />
        <span ref={dragPreviewLabelRef} className="truncate" />
      </div>
    </div>
  );
}

export default memo(SigmaGraph);
