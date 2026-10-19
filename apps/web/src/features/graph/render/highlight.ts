/**
 * Per-node interaction state, encoded as one byte per node and uploaded to
 * the GPU as an R8UI texture. Shaders derive emphasis (size boost, dimming)
 * and edge highlighting from the two endpoint states — so hover/selection
 * costs one O(count) byte-array pass plus a texture upload, never a scene
 * rebuild.
 *
 * Byte layout: low 2 bits = emphasis, bit 2 = hidden.
 */
export const NODE_EMPHASIS_NORMAL = 0
export const NODE_EMPHASIS_DIMMED = 1
export const NODE_EMPHASIS_NEIGHBOR = 2
export const NODE_EMPHASIS_ACTIVE = 3
export const NODE_STATE_HIDDEN_BIT = 4

export interface HighlightInput {
  count: number
  /** Node index under the pointer, or -1. */
  hoverIndex: number
  /** Clicked/selected node index, or -1. */
  selectedIndex: number
  /** CSR adjacency. */
  neighborOffsets: Uint32Array
  neighborIndices: Uint32Array
  /** Hidden node indices (filters / search / neighborhood focus). */
  hiddenIndices: ReadonlySet<number>
}

export interface FocusEdgeHighlight {
  vertices: Float32Array
  selectedVertexCount: number
  hoverVertexCount: number
}

/** Selected first, then hover when the pointer is on a different node. */
export function focusNodeIndices(selectedIndex: number, hoverIndex: number): number[] {
  const indices: number[] = []
  if (selectedIndex >= 0) indices.push(selectedIndex)
  if (hoverIndex >= 0 && hoverIndex !== selectedIndex) indices.push(hoverIndex)
  return indices
}

function markNeighborhood(
  states: Uint8Array,
  index: number,
  neighborOffsets: Uint32Array,
  neighborIndices: Uint32Array,
): void {
  if (index < 0) return
  const end = neighborOffsets[index + 1]!
  for (let cursor = neighborOffsets[index]!; cursor < end; cursor += 1) {
    states[neighborIndices[cursor]!] = NODE_EMPHASIS_NEIGHBOR
  }
}

/**
 * When a node is selected, hovered, or both, each ego-network stays fully
 * visible and the rest of the map dims just enough to keep those links
 * readable. Hover does not replace selection.
 */
export function computeNodeStates(states: Uint8Array, input: HighlightInput): void {
  const hasFocus = input.hoverIndex >= 0 || input.selectedIndex >= 0
  states.fill(hasFocus ? NODE_EMPHASIS_DIMMED : NODE_EMPHASIS_NORMAL, 0, input.count)

  markNeighborhood(states, input.selectedIndex, input.neighborOffsets, input.neighborIndices)
  markNeighborhood(states, input.hoverIndex, input.neighborOffsets, input.neighborIndices)
  if (input.selectedIndex >= 0) states[input.selectedIndex] = NODE_EMPHASIS_ACTIVE
  if (input.hoverIndex >= 0) states[input.hoverIndex] = NODE_EMPHASIS_ACTIVE

  for (const hidden of input.hiddenIndices) {
    if (hidden >= 0 && hidden < input.count) {
      states[hidden] = states[hidden]! | NODE_STATE_HIDDEN_BIT
    }
  }
}

function neighborhoodVertexCount(index: number, neighborOffsets: Uint32Array): number {
  if (index < 0) return 0
  return (neighborOffsets[index + 1]! - neighborOffsets[index]!) * 2
}

function writeNeighborhood(
  vertices: Float32Array,
  floatOffset: number,
  index: number,
  neighborOffsets: Uint32Array,
  neighborIndices: Uint32Array,
): number {
  const start = neighborOffsets[index]!
  const end = neighborOffsets[index + 1]!
  for (let cursor = start; cursor < end; cursor += 1) {
    const neighbor = neighborIndices[cursor]!
    vertices[floatOffset] = index
    vertices[floatOffset + 1] = neighbor
    vertices[floatOffset + 2] = 0
    vertices[floatOffset + 3] = index
    vertices[floatOffset + 4] = neighbor
    vertices[floatOffset + 5] = 1
    floatOffset += 6
  }
  return floatOffset
}

/**
 * Packed highlight geometry: selected neighborhood first (amber pass),
 * then hover neighborhood (cyan pass, drawn on top). Hovering the selected
 * node keeps a single hover-colored range so the two colors stay distinct.
 */
export function buildFocusEdgeVertices(
  selectedIndex: number,
  hoverIndex: number,
  neighborOffsets: Uint32Array,
  neighborIndices: Uint32Array,
): FocusEdgeHighlight {
  const drawSelected = selectedIndex >= 0 && hoverIndex !== selectedIndex
  const selectedVertexCount = drawSelected
    ? neighborhoodVertexCount(selectedIndex, neighborOffsets)
    : 0
  const hoverVertexCount =
    hoverIndex >= 0 ? neighborhoodVertexCount(hoverIndex, neighborOffsets) : 0
  const vertices = new Float32Array((selectedVertexCount + hoverVertexCount) * 3)
  let floatOffset = 0
  if (drawSelected) {
    floatOffset = writeNeighborhood(
      vertices,
      floatOffset,
      selectedIndex,
      neighborOffsets,
      neighborIndices,
    )
  }
  if (hoverIndex >= 0) {
    writeNeighborhood(vertices, floatOffset, hoverIndex, neighborOffsets, neighborIndices)
  }
  return { vertices, selectedVertexCount, hoverVertexCount }
}

/** Translate the id-keyed hidden set from GraphPage into node indices. */
export function hiddenIdsToIndices(
  hiddenIds: ReadonlySet<string>,
  indexById: ReadonlyMap<string, number>,
): Set<number> {
  const result = new Set<number>()
  for (const id of hiddenIds) {
    const index = indexById.get(id)
    if (index !== undefined) result.add(index)
  }
  return result
}
