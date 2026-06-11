import { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { GraphNode } from '@/shared/types';
import { GRAPH_EDGE_RENDER_CAP } from '@/features/graph/model/config';
import { computeGraphLayoutOffThread } from '@/features/graph/workers/graphLayoutClient';
import SigmaGraph from './SigmaGraph';

const { sigmaInstances } = vi.hoisted(() => ({
  sigmaInstances: [] as Array<{
    graph: {
      edges: (node?: string) => string[];
    };
    settings: Record<string, unknown>;
    kill: ReturnType<typeof vi.fn>;
    refresh: ReturnType<typeof vi.fn>;
    emit: (event: string, payload: unknown) => void;
    emitMouse: (event: string, payload: unknown) => void;
  }>,
}));

vi.mock('@sigma/edge-curve', () => ({
  EdgeCurvedArrowProgram: class FakeEdgeCurvedArrowProgram {},
}));

vi.mock('@/features/graph/model/layouts', () => ({
  applyGraphLayout: vi.fn(),
}));

vi.mock('@/features/graph/workers/graphLayoutClient', () => ({
  computeGraphLayoutOffThread: vi.fn(),
}));

vi.mock('graphology', () => {
  class FakeGraph {
    private nodes = new Map<string, Record<string, unknown>>();
    private graphEdges = new Map<string, { source: string; target: string; attributes: Record<string, unknown> }>();
    private edgeCounter = 0;

    get order() {
      return this.nodes.size;
    }

    addNode(id: string, attributes: Record<string, unknown>) {
      this.nodes.set(id, { ...attributes });
    }

    hasNode(id: string) {
      return this.nodes.has(id);
    }

    addEdge(source: string, target: string, attributes: Record<string, unknown>) {
      const id = `e${this.edgeCounter}`;
      this.edgeCounter += 1;
      this.graphEdges.set(id, { source, target, attributes: { ...attributes } });
      return id;
    }

    hasEdge(id: string) {
      return this.graphEdges.has(id);
    }

    edges(node?: string) {
      if (!node) return Array.from(this.graphEdges.keys());
      return Array.from(this.graphEdges.entries())
        .filter(([, edge]) => edge.source === node || edge.target === node)
        .map(([id]) => id);
    }

    forEachEdge(callback: (edge: string, attributes: Record<string, unknown>, source: string, target: string) => void) {
      for (const [id, edge] of this.graphEdges) {
        callback(id, edge.attributes, edge.source, edge.target);
      }
    }

    getNodeAttribute(id: string, key: string) {
      return this.nodes.get(id)?.[key];
    }

    setNodeAttribute(id: string, key: string, value: unknown) {
      const node = this.nodes.get(id);
      if (node) node[key] = value;
    }

    mergeNodeAttributes(id: string, attributes: Record<string, unknown>) {
      const node = this.nodes.get(id);
      if (node) Object.assign(node, attributes);
    }

    removeNodeAttribute(id: string, key: string) {
      delete this.nodes.get(id)?.[key];
    }

    updateEachNodeAttributes(callback: (id: string, attributes: Record<string, unknown>) => Record<string, unknown>) {
      for (const [id, attributes] of this.nodes) {
        this.nodes.set(id, callback(id, { ...attributes }));
      }
    }

    forEachNode(callback: (node: string, attributes: Record<string, unknown>) => void) {
      for (const [id, attributes] of this.nodes) {
        callback(id, attributes);
      }
    }

    listeners() {
      return [];
    }

    removeAllListeners() {}

    on() {}
  }

  return { default: FakeGraph };
});

vi.mock('sigma', () => {
  class FakeSigma {
    graph: { edges: (node?: string) => string[] };
    settings: Record<string, unknown>;
    kill = vi.fn();
    refresh = vi.fn();
    private handlers = new Map<string, Array<(payload: unknown) => void>>();
    private mouseHandlers = new Map<string, Array<(payload: unknown) => void>>();
    private camera = {
      ratio: 1,
      animate: vi.fn(),
      animatedReset: vi.fn(),
      disable: vi.fn(),
      enable: vi.fn(),
      getState: vi.fn(() => ({ x: 0, y: 0, angle: 0, ratio: 1 })),
      on: vi.fn(),
      setState: vi.fn(),
    };
    private mouseCaptor = {
      on: vi.fn((event: string, callback: (payload: unknown) => void) => {
        const callbacks = this.mouseHandlers.get(event) ?? [];
        callbacks.push(callback);
        this.mouseHandlers.set(event, callbacks);
      }),
    };
    private canvases = {
      edges: document.createElement('canvas'),
      edgeLabels: document.createElement('canvas'),
    };

    constructor(graph: { edges: (node?: string) => string[] }, _container: HTMLElement, settings: Record<string, unknown>) {
      this.graph = graph;
      this.settings = { ...settings };
      sigmaInstances.push(this);
    }

    setSetting(name: string, value: unknown) {
      this.settings[name] = value;
    }

    getCamera() {
      return this.camera;
    }

    getMouseCaptor() {
      return this.mouseCaptor;
    }

    getCanvases() {
      return this.canvases;
    }

    graphToViewport(position: { x: number; y: number }) {
      return position;
    }

    viewportToGraph(position: { x: number; y: number }) {
      return position;
    }

    on(event: string, callback: (payload: unknown) => void) {
      const callbacks = this.handlers.get(event) ?? [];
      callbacks.push(callback);
      this.handlers.set(event, callbacks);
    }

    emit(event: string, payload: unknown) {
      for (const callback of this.handlers.get(event) ?? []) callback(payload);
    }

    emitMouse(event: string, payload: unknown) {
      for (const callback of this.mouseHandlers.get(event) ?? []) callback(payload);
    }
  }

  return { default: FakeSigma };
});

function graphNode(id: string, type: GraphNode['type'] = 'entity'): GraphNode {
  return {
    id,
    label: id,
    type,
    properties: {},
    edgeCount: 1,
  };
}

function createFakeWebGL2Context(options: { textureUploadError?: boolean } = {}) {
  let textureUploadChecked = false;
  return {
    ARRAY_BUFFER: 0x8892,
    BLEND: 0x0be2,
    COLOR_BUFFER_BIT: 0x4000,
    CLAMP_TO_EDGE: 0x812f,
    COMPILE_STATUS: 0x8b81,
    DEPTH_TEST: 0x0b71,
    FLOAT: 0x1406,
    FRAGMENT_SHADER: 0x8b30,
    LINEAR: 0x2601,
    LINES: 0x0001,
    LINK_STATUS: 0x8b82,
    MAX_TEXTURE_SIZE: 0x0d33,
    NEAREST: 0x2600,
    NO_ERROR: 0,
    ONE_MINUS_SRC_ALPHA: 0x0303,
    RGBA: 0x1908,
    RGBA32F: 0x8814,
    SRC_ALPHA: 0x0302,
    STATIC_DRAW: 0x88e4,
    TEXTURE0: 0x84c0,
    TEXTURE_2D: 0x0de1,
    TEXTURE_MAG_FILTER: 0x2800,
    TEXTURE_MIN_FILTER: 0x2801,
    TEXTURE_WRAP_S: 0x2802,
    TEXTURE_WRAP_T: 0x2803,
    VERTEX_SHADER: 0x8b31,
    activeTexture: vi.fn(),
    attachShader: vi.fn(),
    bindBuffer: vi.fn(),
    bindTexture: vi.fn(),
    blendFunc: vi.fn(),
    bufferData: vi.fn(),
    clear: vi.fn(),
    clearColor: vi.fn(),
    compileShader: vi.fn(),
    createBuffer: vi.fn(() => ({})),
    createProgram: vi.fn(() => ({})),
    createShader: vi.fn(() => ({})),
    createTexture: vi.fn(() => ({})),
    deleteBuffer: vi.fn(),
    deleteProgram: vi.fn(),
    deleteShader: vi.fn(),
    deleteTexture: vi.fn(),
    disable: vi.fn(),
    enable: vi.fn(),
    enableVertexAttribArray: vi.fn(),
    getAttribLocation: vi.fn(() => 0),
    getError: vi.fn(() => {
      if (options.textureUploadError && !textureUploadChecked) {
        textureUploadChecked = true;
        return 1;
      }
      return 0;
    }),
    getParameter: vi.fn(() => 4096),
    getProgramParameter: vi.fn(() => true),
    getShaderParameter: vi.fn(() => true),
    getUniformLocation: vi.fn(() => ({})),
    linkProgram: vi.fn(),
    pixelStorei: vi.fn(),
    shaderSource: vi.fn(),
    texImage2D: vi.fn(),
    texParameteri: vi.fn(),
    texSubImage2D: vi.fn(),
    uniform1f: vi.fn(),
    uniform1i: vi.fn(),
    uniform4f: vi.fn(),
    uniformMatrix3fv: vi.fn(),
    useProgram: vi.fn(),
    vertexAttribPointer: vi.fn(),
    viewport: vi.fn(),
    drawArrays: vi.fn(),
  } as unknown as WebGL2RenderingContext & {
    bufferData: ReturnType<typeof vi.fn>;
    texImage2D: ReturnType<typeof vi.fn>;
    texSubImage2D: ReturnType<typeof vi.fn>;
  };
}

async function flushGraphEffects() {
  for (let i = 0; i < 4; i += 1) {
    await act(async () => {
      await Promise.resolve();
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
  }
}

async function flushAnimationFrames(count: number) {
  for (let i = 0; i < count; i += 1) {
    await act(async () => {
      await new Promise((resolve) => requestAnimationFrame(resolve));
    });
  }
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

function denseGraphFixture(nodeCount = 15001, edgeCount = 10000) {
  const nodes = Array.from({ length: nodeCount }, (_, index) => graphNode(`node-${index}`));
  const edges = Array.from({ length: edgeCount }, (_, index) => ({
    id: `edge-${index}`,
    sourceId: `node-${index % 100}`,
    targetId: `node-${100 + Math.floor(index / 100)}`,
    label: '',
    weight: 1,
  }));
  const horizontal = new Float32Array(nodes.length * 2);
  const vertical = new Float32Array(nodes.length * 2);
  const diagonal = new Float32Array(nodes.length * 2);
  for (let i = 0; i < nodes.length; i += 1) {
    horizontal[i * 2] = i;
    horizontal[i * 2 + 1] = 0;
    vertical[i * 2] = 0;
    vertical[i * 2 + 1] = i;
    diagonal[i * 2] = i;
    diagonal[i * 2 + 1] = i;
  }
  return { nodes, edges, horizontal, vertical, diagonal };
}

describe('SigmaGraph', () => {
  let container: HTMLDivElement;
  let root: Root;

  beforeEach(() => {
    vi.clearAllMocks();
    Object.defineProperty(window.navigator, 'userAgent', {
      value: 'jsdom',
      configurable: true,
    });
    sigmaInstances.length = 0;
    vi.mocked(computeGraphLayoutOffThread).mockReset();
    (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    container = document.createElement('div');
    document.body.appendChild(container);
    root = createRoot(container);
  });

  afterEach(() => {
    act(() => {
      root.unmount();
    });
    container.remove();
    vi.restoreAllMocks();
  });

  it('keeps selection and hidden reducers when edge density changes without rebuilding', async () => {
    const nodes = [graphNode('a'), graphNode('b'), graphNode('c', 'document')];
    const edges = [
      { id: 'ab', sourceId: 'a', targetId: 'b', label: '', weight: 1 },
      { id: 'bc', sourceId: 'b', targetId: 'c', label: '', weight: 1 },
    ];
    const hiddenIds = new Set(['c']);
    const onSelect = vi.fn();

    await act(async () => {
      root.render(
        <SigmaGraph
          nodes={nodes}
          edges={edges}
          selectedId="a"
          onSelect={onSelect}
          layout="bands"
          hiddenIds={hiddenIds}
          showDenseEdges={false}
        />,
      );
    });
    await flushGraphEffects();

    await act(async () => {
      root.render(
        <SigmaGraph
          nodes={nodes}
          edges={edges}
          selectedId="a"
          onSelect={onSelect}
          layout="bands"
          hiddenIds={hiddenIds}
          showDenseEdges
        />,
      );
    });
    await flushGraphEffects();

    expect(sigmaInstances).toHaveLength(1);
    const sigma = sigmaInstances[0];
    const nodeReducer = sigma.settings.nodeReducer as (node: string, data: Record<string, unknown>) => Record<string, unknown>;
    const edgeReducer = sigma.settings.edgeReducer as (edge: string, data: Record<string, unknown>) => Record<string, unknown>;

    expect(nodeReducer).toBeTypeOf('function');
    expect(edgeReducer).toBeTypeOf('function');
    expect(nodeReducer('a', { size: 1, label: 'a' })).toMatchObject({
      highlighted: true,
      forceLabel: true,
    });
    expect(nodeReducer('c', { size: 1, label: 'c' })).toMatchObject({
      hidden: true,
      label: '',
    });

    const hiddenEdge = sigma.graph.edges('c')[0];
    expect(edgeReducer(hiddenEdge, { size: 1 })).toMatchObject({ hidden: true });
  });

  it('uses the indexed WebGL all-edge layer for dense full topology rendering', async () => {
    Object.defineProperty(window.navigator, 'userAgent', {
      value: 'Firefox',
      configurable: true,
    });
    const gl = createFakeWebGL2Context();
    vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockImplementation(((contextId: string) => {
      if (contextId === 'webgl2') return gl;
      return null;
    }) as HTMLCanvasElement['getContext']);
    const nodes = [graphNode('a'), graphNode('b')];
    const edges = Array.from({ length: GRAPH_EDGE_RENDER_CAP + 1 }, (_, index) => ({
      id: `edge-${index}`,
      sourceId: 'a',
      targetId: 'b',
      label: '',
      weight: 1,
    }));

    await act(async () => {
      root.render(
        <SigmaGraph
          nodes={nodes}
          edges={edges}
          selectedId={null}
          onSelect={vi.fn()}
          layout="bands"
          showDenseEdges={false}
        />,
      );
    });
    await flushGraphEffects();

    const allEdgesCanvas = container.querySelector('canvas[data-all-edges-layer="indexed"]');
    expect(allEdgesCanvas).not.toBeNull();
    expect(allEdgesCanvas?.getAttribute('data-all-edges-count')).toBe(String(edges.length));
    expect(gl.bufferData).toHaveBeenCalled();
  });

  it('falls back to coordinate all-edge rendering on the existing context when indexed upload fails', async () => {
    Object.defineProperty(window.navigator, 'userAgent', {
      value: 'Firefox',
      configurable: true,
    });
    const gl = createFakeWebGL2Context({ textureUploadError: true });
    vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockImplementation(((contextId: string) => {
      if (contextId === 'webgl2') return gl;
      return null;
    }) as HTMLCanvasElement['getContext']);
    const nodes = [graphNode('a'), graphNode('b')];
    const edges = Array.from({ length: GRAPH_EDGE_RENDER_CAP + 1 }, (_, index) => ({
      id: `edge-${index}`,
      sourceId: 'a',
      targetId: 'b',
      label: '',
      weight: 1,
    }));

    await act(async () => {
      root.render(
        <SigmaGraph
          nodes={nodes}
          edges={edges}
          selectedId={null}
          onSelect={vi.fn()}
          layout="bands"
          showDenseEdges={false}
        />,
      );
    });
    await flushGraphEffects();

    const allEdgesCanvas = container.querySelector('canvas[data-all-edges-layer="coordinate"]');
    expect(allEdgesCanvas).not.toBeNull();
    expect(allEdgesCanvas?.getAttribute('data-all-edges-count')).toBe(String(edges.length));
    expect(gl.getError).toHaveBeenCalled();
    expect(gl.bufferData).toHaveBeenCalled();
  });

  it('updates only the node-position texture while dragging dense full-edge graphs', async () => {
    Object.defineProperty(window.navigator, 'userAgent', {
      value: 'Firefox',
      configurable: true,
    });
    const gl = createFakeWebGL2Context();
    vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockImplementation(((contextId: string) => {
      if (contextId === 'webgl2') return gl;
      return null;
    }) as HTMLCanvasElement['getContext']);
    const nodes = Array.from({ length: 15001 }, (_, index) => graphNode(`node-${index}`));
    const edges = Array.from({ length: GRAPH_EDGE_RENDER_CAP + 1 }, (_, index) => ({
      id: `edge-${index}`,
      sourceId: 'node-0',
      targetId: 'node-1',
      label: '',
      weight: 1,
    }));

    await act(async () => {
      root.render(
        <SigmaGraph
          nodes={nodes}
          edges={edges}
          selectedId={null}
          onSelect={vi.fn()}
          layout="bands"
          showDenseEdges={false}
        />,
      );
    });
    await flushGraphEffects();

    const sigma = sigmaInstances[0];
    const allEdgesCanvas = container.querySelector('canvas[data-all-edges-layer="indexed"]');
    expect(allEdgesCanvas).not.toBeNull();

    await act(async () => {
      sigma.emit('downNode', { node: 'node-0' });
      sigma.emitMouse('mousemovebody', {
        x: 10,
        y: 20,
        original: {
          clientX: 100,
          clientY: 120,
          preventDefault: vi.fn(),
          stopPropagation: vi.fn(),
        },
        preventSigmaDefault: vi.fn(),
      });
    });
    await act(async () => {
      await new Promise((resolve) => requestAnimationFrame(resolve));
    });

    expect(gl.texSubImage2D).toHaveBeenCalled();
    gl.bufferData.mockClear();

    await act(async () => {
      sigma.emitMouse('mouseup', {});
    });
    await act(async () => {
      await new Promise((resolve) => requestAnimationFrame(resolve));
    });

    expect(gl.bufferData).not.toHaveBeenCalled();
    expect(allEdgesCanvas?.getAttribute('data-all-edges-layer')).toBe('indexed');
  });

  it('updates the indexed all-edge texture without rebuilding the edge buffer on dense layout switch', async () => {
    Object.defineProperty(window.navigator, 'userAgent', {
      value: 'Firefox',
      configurable: true,
    });
    const gl = createFakeWebGL2Context();
    vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockImplementation(((contextId: string) => {
      if (contextId === 'webgl2') return gl;
      return null;
    }) as HTMLCanvasElement['getContext']);
    const { nodes, edges, horizontal, vertical } = denseGraphFixture();
    const mockedComputeLayout = vi.mocked(computeGraphLayoutOffThread);
    mockedComputeLayout
      .mockResolvedValueOnce({ type: 'result', requestId: 1, positions: horizontal, elapsedMs: 1 })
      .mockResolvedValueOnce({ type: 'result', requestId: 2, positions: vertical, elapsedMs: 1 });

    await act(async () => {
      root.render(
        <SigmaGraph
          nodes={nodes}
          edges={edges}
          selectedId={null}
          onSelect={vi.fn()}
          layout="bands"
          showDenseEdges={false}
        />,
      );
    });
    await flushGraphEffects();

    const sigma = sigmaInstances[0];
    const allEdgesCanvas = container.querySelector('canvas[data-all-edges-layer="indexed"]');
    expect(allEdgesCanvas).not.toBeNull();
    expect(allEdgesCanvas?.getAttribute('data-all-edges-count')).toBe(String(edges.length));

    gl.bufferData.mockClear();
    gl.texImage2D.mockClear();
    gl.texSubImage2D.mockClear();
    sigma.refresh.mockClear();
    sigma.kill.mockClear();
    const graphEdgesSpy = vi.spyOn(sigma.graph, 'edges');
    graphEdgesSpy.mockClear();

    await act(async () => {
      root.render(
        <SigmaGraph
          nodes={nodes}
          edges={edges}
          selectedId={null}
          onSelect={vi.fn()}
          layout="components"
          showDenseEdges={false}
        />,
      );
    });
    await flushGraphEffects();
    await flushAnimationFrames(140);

    expect(sigmaInstances).toHaveLength(1);
    expect(sigma.kill).not.toHaveBeenCalled();
    expect(mockedComputeLayout).toHaveBeenLastCalledWith(expect.objectContaining({ layout: 'components' }));
    expect(graphEdgesSpy).not.toHaveBeenCalled();
    expect(gl.bufferData).not.toHaveBeenCalled();
    expect(gl.texImage2D).not.toHaveBeenCalled();
    const textureUploadCalls = gl.texSubImage2D.mock.calls.filter((call) => call.length >= 9);
    expect(textureUploadCalls.length).toBeGreaterThan(1);
    expect(textureUploadCalls.some((call) => Number(call[3]) > 0)).toBe(true);
    expect(textureUploadCalls.every((call) => Number(call[5]) <= 4)).toBe(true);
    const edgeRefreshCalls = sigma.refresh.mock.calls
      .map(([options]) => options as { partialGraph?: { edges?: unknown[] }; schedule?: boolean; skipIndexation?: boolean })
      .filter((options) => options.partialGraph?.edges?.length);
    expect(edgeRefreshCalls).toHaveLength(0);
    const nodeRefreshCalls = sigma.refresh.mock.calls
      .map(([options]) => options as { partialGraph?: { nodes?: unknown[] }; schedule?: boolean; skipIndexation?: boolean })
      .filter((options) => options.schedule === true && options.partialGraph?.nodes?.length);
    expect(nodeRefreshCalls.length).toBeGreaterThan(1);
    expect(nodeRefreshCalls.every((options) => options.skipIndexation === true)).toBe(true);
    expect(nodeRefreshCalls.every((options) => (options.partialGraph?.nodes?.length ?? 0) <= 5000)).toBe(true);
    expect(
      sigma.refresh.mock.calls.some(([options]) => {
        const refreshOptions = options as { partialGraph?: unknown; schedule?: boolean; skipIndexation?: boolean };
        return refreshOptions.schedule === true && refreshOptions.skipIndexation === true && !refreshOptions.partialGraph;
      }),
    ).toBe(false);
    expect(allEdgesCanvas?.getAttribute('data-all-edges-layer')).toBe('indexed');
    expect(allEdgesCanvas?.getAttribute('data-all-edges-count')).toBe(String(edges.length));
  });

  it('refreshes sampled Sigma edges on dense layout switch when the indexed all-edge overlay is inactive', async () => {
    Object.defineProperty(window.navigator, 'userAgent', {
      value: 'Firefox',
      configurable: true,
    });
    vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockImplementation(() => null);
    const { nodes, edges, horizontal, vertical } = denseGraphFixture();
    const mockedComputeLayout = vi.mocked(computeGraphLayoutOffThread);
    mockedComputeLayout
      .mockResolvedValueOnce({ type: 'result', requestId: 1, positions: horizontal, elapsedMs: 1 })
      .mockResolvedValueOnce({ type: 'result', requestId: 2, positions: vertical, elapsedMs: 1 });

    await act(async () => {
      root.render(
        <SigmaGraph
          nodes={nodes}
          edges={edges}
          selectedId={null}
          onSelect={vi.fn()}
          layout="bands"
          showDenseEdges={false}
        />,
      );
    });
    await flushGraphEffects();

    const sigma = sigmaInstances[0];
    expect(container.querySelector('canvas[data-all-edges-layer]')).toBeNull();
    sigma.refresh.mockClear();

    await act(async () => {
      root.render(
        <SigmaGraph
          nodes={nodes}
          edges={edges}
          selectedId={null}
          onSelect={vi.fn()}
          layout="components"
          showDenseEdges={false}
        />,
      );
    });
    await flushGraphEffects();
    await flushAnimationFrames(140);

    const edgeRefreshCalls = sigma.refresh.mock.calls
      .map(([options]) => options as { partialGraph?: { edges?: unknown[] }; schedule?: boolean; skipIndexation?: boolean })
      .filter((options) => options.partialGraph?.edges?.length);
    expect(edgeRefreshCalls.length).toBeGreaterThan(1);
    expect(edgeRefreshCalls.every((options) => options.schedule === true)).toBe(true);
    expect(edgeRefreshCalls.every((options) => options.skipIndexation === true)).toBe(true);
    expect(edgeRefreshCalls.every((options) => (options.partialGraph?.edges?.length ?? 0) <= 4000)).toBe(true);
  });

  it('keeps the indexed all-edge overlay hidden and undrawn while a dense layout switch is suspended', async () => {
    Object.defineProperty(window.navigator, 'userAgent', {
      value: 'Firefox',
      configurable: true,
    });
    vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockReturnValue(new DOMRect(0, 0, 1200, 800));
    const gl = createFakeWebGL2Context();
    vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockImplementation(((contextId: string) => {
      if (contextId === 'webgl2') return gl;
      return null;
    }) as HTMLCanvasElement['getContext']);

    const { nodes, edges, horizontal, vertical } = denseGraphFixture();
    const pendingSwitch = deferred<Awaited<ReturnType<typeof computeGraphLayoutOffThread>>>();
    const mockedComputeLayout = vi.mocked(computeGraphLayoutOffThread);
    mockedComputeLayout
      .mockResolvedValueOnce({ type: 'result', requestId: 1, positions: horizontal, elapsedMs: 1 })
      .mockReturnValueOnce(pendingSwitch.promise);

    await act(async () => {
      root.render(
        <SigmaGraph
          nodes={nodes}
          edges={edges}
          selectedId={null}
          onSelect={vi.fn()}
          layout="bands"
          showDenseEdges={false}
        />,
      );
    });
    await flushGraphEffects();
    await flushAnimationFrames(4);

    const sigma = sigmaInstances[0];
    const allEdgesCanvas = container.querySelector('canvas[data-all-edges-layer="indexed"]');
    expect(allEdgesCanvas).not.toBeNull();
    gl.drawArrays.mockClear();

    await act(async () => {
      root.render(
        <SigmaGraph
          nodes={nodes}
          edges={edges}
          selectedId={null}
          onSelect={vi.fn()}
          layout="components"
          showDenseEdges={false}
        />,
      );
    });
    await flushGraphEffects();

    expect(allEdgesCanvas?.style.visibility).toBe('hidden');
    sigma.emit('afterRender', {});
    await flushAnimationFrames(2);
    expect(gl.drawArrays).not.toHaveBeenCalled();

    await act(async () => {
      pendingSwitch.resolve({ type: 'result', requestId: 2, positions: vertical, elapsedMs: 1 });
      await Promise.resolve();
    });
    await flushAnimationFrames(140);

    expect(allEdgesCanvas?.style.visibility).toBe('');
    expect(gl.drawArrays).toHaveBeenCalled();
  });

  it('does not let a superseded dense layout switch re-enable the all-edge overlay', async () => {
    Object.defineProperty(window.navigator, 'userAgent', {
      value: 'Firefox',
      configurable: true,
    });
    vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockReturnValue(new DOMRect(0, 0, 1200, 800));
    const gl = createFakeWebGL2Context();
    vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockImplementation(((contextId: string) => {
      if (contextId === 'webgl2') return gl;
      return null;
    }) as HTMLCanvasElement['getContext']);

    const { nodes, edges, horizontal, vertical, diagonal } = denseGraphFixture();
    const firstSwitch = deferred<Awaited<ReturnType<typeof computeGraphLayoutOffThread>>>();
    const secondSwitch = deferred<Awaited<ReturnType<typeof computeGraphLayoutOffThread>>>();
    const mockedComputeLayout = vi.mocked(computeGraphLayoutOffThread);
    mockedComputeLayout
      .mockResolvedValueOnce({ type: 'result', requestId: 1, positions: horizontal, elapsedMs: 1 })
      .mockReturnValueOnce(firstSwitch.promise)
      .mockReturnValueOnce(secondSwitch.promise);

    await act(async () => {
      root.render(
        <SigmaGraph
          nodes={nodes}
          edges={edges}
          selectedId={null}
          onSelect={vi.fn()}
          layout="bands"
          showDenseEdges={false}
        />,
      );
    });
    await flushGraphEffects();
    await flushAnimationFrames(4);

    const sigma = sigmaInstances[0];
    const allEdgesCanvas = container.querySelector('canvas[data-all-edges-layer="indexed"]');
    expect(allEdgesCanvas).not.toBeNull();
    gl.drawArrays.mockClear();

    await act(async () => {
      root.render(
        <SigmaGraph
          nodes={nodes}
          edges={edges}
          selectedId={null}
          onSelect={vi.fn()}
          layout="components"
          showDenseEdges={false}
        />,
      );
    });
    await flushGraphEffects();
    expect(allEdgesCanvas?.style.visibility).toBe('hidden');

    await act(async () => {
      root.render(
        <SigmaGraph
          nodes={nodes}
          edges={edges}
          selectedId={null}
          onSelect={vi.fn()}
          layout="rings"
          showDenseEdges={false}
        />,
      );
    });
    await flushGraphEffects();
    expect(allEdgesCanvas?.style.visibility).toBe('hidden');

    await act(async () => {
      firstSwitch.resolve({ type: 'result', requestId: 2, positions: vertical, elapsedMs: 1 });
      await Promise.resolve();
    });
    await flushAnimationFrames(4);
    sigma.emit('afterRender', {});
    await flushAnimationFrames(2);

    expect(allEdgesCanvas?.style.visibility).toBe('hidden');
    expect(gl.drawArrays).not.toHaveBeenCalled();

    await act(async () => {
      secondSwitch.resolve({ type: 'result', requestId: 3, positions: diagonal, elapsedMs: 1 });
      await Promise.resolve();
    });
    await flushAnimationFrames(140);

    expect(allEdgesCanvas?.style.visibility).toBe('');
    expect(gl.drawArrays).toHaveBeenCalled();
  });

  it('clears all-edge overlay suspension when topology changes during a dense layout switch', async () => {
    Object.defineProperty(window.navigator, 'userAgent', {
      value: 'Firefox',
      configurable: true,
    });
    vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockReturnValue(new DOMRect(0, 0, 1200, 800));
    const gl = createFakeWebGL2Context();
    vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockImplementation(((contextId: string) => {
      if (contextId === 'webgl2') return gl;
      return null;
    }) as HTMLCanvasElement['getContext']);

    const initial = denseGraphFixture();
    const next = denseGraphFixture(15002, 10001);
    const pendingSwitch = deferred<Awaited<ReturnType<typeof computeGraphLayoutOffThread>>>();
    const mockedComputeLayout = vi.mocked(computeGraphLayoutOffThread);
    mockedComputeLayout
      .mockResolvedValueOnce({ type: 'result', requestId: 1, positions: initial.horizontal, elapsedMs: 1 })
      .mockReturnValueOnce(pendingSwitch.promise)
      .mockResolvedValueOnce({ type: 'result', requestId: 3, positions: next.diagonal, elapsedMs: 1 });

    await act(async () => {
      root.render(
        <SigmaGraph
          nodes={initial.nodes}
          edges={initial.edges}
          selectedId={null}
          onSelect={vi.fn()}
          layout="bands"
          showDenseEdges={false}
        />,
      );
    });
    await flushGraphEffects();
    await flushAnimationFrames(4);

    const initialSigma = sigmaInstances[0];
    const allEdgesCanvas = container.querySelector('canvas[data-all-edges-layer="indexed"]');
    expect(allEdgesCanvas).not.toBeNull();

    await act(async () => {
      root.render(
        <SigmaGraph
          nodes={initial.nodes}
          edges={initial.edges}
          selectedId={null}
          onSelect={vi.fn()}
          layout="components"
          showDenseEdges={false}
        />,
      );
    });
    await flushGraphEffects();

    expect(allEdgesCanvas?.style.visibility).toBe('hidden');

    await act(async () => {
      root.render(
        <SigmaGraph
          nodes={next.nodes}
          edges={next.edges}
          selectedId={null}
          onSelect={vi.fn()}
          layout="components"
          showDenseEdges={false}
        />,
      );
    });
    await flushGraphEffects();
    await flushAnimationFrames(8);

    expect(sigmaInstances.length).toBeGreaterThanOrEqual(2);
    expect(initialSigma.kill).toHaveBeenCalled();
    expect(allEdgesCanvas?.style.visibility).toBe('');
    expect(allEdgesCanvas?.getAttribute('data-all-edges-count')).toBe(String(next.edges.length));

    const nextSigma = sigmaInstances[sigmaInstances.length - 1];
    gl.drawArrays.mockClear();
    nextSigma.emit('afterRender', {});
    await flushAnimationFrames(2);
    expect(gl.drawArrays).toHaveBeenCalled();

    await act(async () => {
      pendingSwitch.resolve({ type: 'result', requestId: 2, positions: initial.vertical, elapsedMs: 1 });
      await Promise.resolve();
    });
    await flushAnimationFrames(4);

    expect(allEdgesCanvas?.style.visibility).toBe('');
    nextSigma.emit('afterRender', {});
    await flushAnimationFrames(2);
    expect(gl.drawArrays).toHaveBeenCalled();
  });
});
