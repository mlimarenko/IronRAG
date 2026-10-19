import { LoaderCircle, MonitorX } from 'lucide-react'
import { useEffect, useMemo, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useApp } from '@/shared/contexts/app-context'
import { formatNumber } from '@/shared/format'
import type { GraphEdge, GraphNode } from '@/shared/types'
import { GRAPH_EDGE_RENDER_CAP, type GraphLayoutType } from './model/config'
import { hiddenIdsToIndices } from './render/highlight'
import { buildSceneModel, type GraphSceneModel } from './render/sceneModel'
import { GraphRuntime } from './render/graphRuntime'

type GraphCanvasProps = Readonly<{
  nodes: GraphNode[]
  edges: GraphEdge[]
  hiddenIds: ReadonlySet<string>
  selectedId: string | null
  layout: GraphLayoutType
  showDenseEdges: boolean
  showNoMatch: boolean
  onSelect: (id: string | null) => void
  onFitViewReady: (fit: () => void) => void
  onLayoutBusyChange?: ((busy: boolean) => void) | undefined
}>

declare global {
  interface Window {
    __IRONRAG_GRAPH_BENCH__?: boolean
    __graphDebug?: {
      screenPositionOf(kind: 'hub' | number): { x: number; y: number } | null
      drawnNodeCount(): number
      drawnEdgeCount(): number
      hoverStats(): { count: number; p50: number; p95: number; max: number }
    }
  }
}

/**
 * WebGL2 graph canvas. The React component only owns the DOM shell and prop
 * plumbing; rendering, camera, picking, labels, and layout streaming live in
 * `GraphRuntime` (see `render/graphRuntime.ts`). The scene is fully
 * typed-array based, so the component neither re-renders on camera events
 * nor rebuilds anything when hover/selection changes.
 */
export function GraphCanvas({
  nodes,
  edges,
  hiddenIds,
  selectedId,
  layout,
  showDenseEdges,
  showNoMatch,
  onSelect,
  onFitViewReady,
  onLayoutBusyChange,
}: GraphCanvasProps) {
  const { t } = useTranslation()
  const { locale } = useApp()
  const containerRef = useRef<HTMLDivElement>(null)
  const glCanvasRef = useRef<HTMLCanvasElement>(null)
  const labelCanvasRef = useRef<HTMLCanvasElement>(null)
  const runtimeRef = useRef<GraphRuntime | null>(null)
  const appliedLayoutRef = useRef<GraphLayoutType | null>(null)
  // Model-keyed lifecycle states, set exclusively from runtime callbacks and
  // scheduled tasks (never synchronously inside the mount effect): the model
  // whose first frame has been painted, and the model that failed WebGL2
  // context creation.
  const [paintedModel, setPaintedModel] = useState<GraphSceneModel | null>(null)
  const [unsupportedModel, setUnsupportedModel] = useState<GraphSceneModel | null>(null)
  const [layoutSwitchBusy, setLayoutSwitchBusy] = useState(false)

  const model = useMemo(() => buildSceneModel(nodes, edges), [nodes, edges])
  const isUnsupported = unsupportedModel === model
  const isPreparing = model.count > 0 && paintedModel !== model && !isUnsupported

  const onSelectRef = useRef(onSelect)
  const onLayoutBusyChangeRef = useRef(onLayoutBusyChange)
  const layoutRef = useRef(layout)
  useEffect(() => {
    onSelectRef.current = onSelect
    onLayoutBusyChangeRef.current = onLayoutBusyChange
    layoutRef.current = layout
  }, [onSelect, onLayoutBusyChange, layout])

  useEffect(() => {
    const container = containerRef.current
    const glCanvas = glCanvasRef.current
    const labelCanvas = labelCanvasRef.current
    if (!container || !glCanvas || !labelCanvas || model.count === 0) return

    let runtime: GraphRuntime
    try {
      runtime = new GraphRuntime({
        model,
        glCanvas,
        labelCanvas,
        callbacks: {
          onSelect: (id) => onSelectRef.current(id),
          onLayoutBusyChange: (busy) => {
            setLayoutSwitchBusy(busy)
            onLayoutBusyChangeRef.current?.(busy)
          },
          onFirstFrame: () => {
            performance.mark('graph:first-frame')
            setPaintedModel(model)
          },
        },
      })
    } catch {
      // jsdom or a WebGL2-less browser: show the capability notice instead
      // of a dead canvas.
      queueMicrotask(() => setUnsupportedModel(model))
      return
    }
    runtimeRef.current = runtime

    const measure = (): void => {
      const rect = container.getBoundingClientRect()
      runtime.resize(rect.width, rect.height, window.devicePixelRatio || 1)
    }
    measure()
    const resizeObserver = new ResizeObserver(measure)
    resizeObserver.observe(container)

    appliedLayoutRef.current = layoutRef.current
    runtime.applyLayout(layoutRef.current, 'initial')
    onFitViewReady(() => runtime.fitView())

    if (import.meta.env.DEV || window.__IRONRAG_GRAPH_BENCH__) {
      window.__graphDebug = {
        screenPositionOf: (kind) =>
          runtime.screenPositionOfNode(kind === 'hub' ? model.maxDegreeIndex : kind),
        drawnNodeCount: () => model.count,
        drawnEdgeCount: () => model.edgeCount,
        hoverStats: () => runtime.hoverStats(),
      }
    }

    return () => {
      resizeObserver.disconnect()
      runtime.dispose()
      if (runtimeRef.current === runtime) runtimeRef.current = null
      appliedLayoutRef.current = null
    }
  }, [model, onFitViewReady])

  useEffect(() => {
    const runtime = runtimeRef.current
    if (!runtime || appliedLayoutRef.current === layout) return
    appliedLayoutRef.current = layout
    runtime.applyLayout(layout, 'switch')
  }, [layout])

  useEffect(() => {
    runtimeRef.current?.setHiddenIndices(hiddenIdsToIndices(hiddenIds, model.indexById))
  }, [hiddenIds, model])

  useEffect(() => {
    runtimeRef.current?.setSelectedId(selectedId)
  }, [selectedId, model])

  useEffect(() => {
    const cap = showDenseEdges ? model.edgeCount : Math.min(model.edgeCount, GRAPH_EDGE_RENDER_CAP)
    runtimeRef.current?.setEdgeDrawCount(cap)
  }, [showDenseEdges, model])

  return (
    <div className="graph-canvas">
      <div className="graph-canvas__runtime" ref={containerRef}>
        <canvas className="graph-canvas__gl" ref={glCanvasRef} />
        {/* Painted overlay, never a control: hidden from assistive
            technology and explicitly out of the tab order. */}
        <canvas
          className="graph-canvas__labels"
          ref={labelCanvasRef}
          aria-hidden="true"
          tabIndex={-1}
        />
      </div>
      {isUnsupported && (
        <div className="graph-canvas__preparing" role="alert">
          <div className="graph-canvas__preparing-content">
            <MonitorX size={22} />
            <strong>{t('graph.webglRequired')}</strong>
          </div>
        </div>
      )}
      {isPreparing && !isUnsupported && (
        <div className="graph-canvas__preparing" aria-live="polite">
          <div className="graph-canvas__preparing-content">
            <LoaderCircle className="spin" size={22} />
            <strong>{t('graph.recomputingLayout')}</strong>
            <span>
              {formatNumber(nodes.length, locale)} {t('graph.nodes')},{' '}
              {formatNumber(edges.length, locale)} {t('graph.edges')}
            </span>
          </div>
        </div>
      )}
      {showNoMatch && <div className="graph-no-match">{t('graph.noMatchingNodes')}</div>}
      {layoutSwitchBusy && !isPreparing && (
        <div className="graph-layout-busy">{t('graph.recomputingLayout')}</div>
      )}
    </div>
  )
}
