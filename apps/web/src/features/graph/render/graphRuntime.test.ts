import { afterEach, expect, it, vi } from 'vitest'
import { GraphRuntime } from './graphRuntime'
import type { GraphSceneModel } from './sceneModel'

vi.mock('./webglRenderer', () => ({
  createGraphRenderer: () => ({ dispose: vi.fn() }),
}))

const originalFonts = Object.getOwnPropertyDescriptor(document, 'fonts')

afterEach(() => {
  if (originalFonts) Object.defineProperty(document, 'fonts', originalFonts)
  else Reflect.deleteProperty(document, 'fonts')
  vi.unstubAllGlobals()
  vi.restoreAllMocks()
})

it('redraws when fonts finish loading and stops observing them after disposal', async () => {
  const finishLoads: Array<(fonts: FontFace[]) => void> = []
  const fonts = new EventTarget() as FontFaceSet
  fonts.load = vi.fn(() => new Promise<FontFace[]>((resolve) => finishLoads.push(resolve)))
  Object.defineProperty(document, 'fonts', { configurable: true, value: fonts })
  const requestFrame = vi.fn(() => 1)
  vi.stubGlobal('requestAnimationFrame', requestFrame)
  vi.stubGlobal('cancelAnimationFrame', vi.fn())

  const model: GraphSceneModel = {
    count: 0,
    ids: [],
    indexById: new Map(),
    labels: [],
    types: [],
    sizes: new Float32Array(),
    colors: new Uint8Array(),
    degrees: new Uint32Array(),
    seedPositions: new Float32Array(),
    neighborOffsets: new Uint32Array(),
    neighborIndices: new Uint32Array(),
    edgeCount: 0,
    edgeEndpoints: new Uint32Array(),
    edgeWeights: new Float32Array(),
    maxDegreeIndex: -1,
    topologyKey: 'empty',
  }
  const labelCanvas = document.createElement('canvas')
  labelCanvas.style.fontFamily = '"Example Canvas Family", serif'
  const options = {
    model,
    glCanvas: document.createElement('canvas'),
    labelCanvas,
    callbacks: { onSelect: vi.fn(), onLayoutBusyChange: vi.fn(), onFirstFrame: vi.fn() },
  }
  const runtime = new GraphRuntime(options)
  expect(fonts.load).toHaveBeenCalledWith('11px "Example Canvas Family", serif')

  finishLoads[0]!([])
  await Promise.resolve()
  expect(requestFrame).toHaveBeenCalledTimes(1)
  runtime.dispose()

  const disposedRuntime = new GraphRuntime(options)
  disposedRuntime.dispose()
  fonts.dispatchEvent(new Event('loadingdone'))
  finishLoads[1]!([])
  await Promise.resolve()
  expect(requestFrame).toHaveBeenCalledTimes(1)
})
