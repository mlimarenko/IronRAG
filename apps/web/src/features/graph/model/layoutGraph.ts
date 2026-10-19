/**
 * Narrow, typed-array-backed stand-in for the graphology `Graph` surface the
 * geometric layouts consume. The former pipeline built a full graphology
 * instance per layout request — measured at ~2.5 s build time and ~340 MB
 * retained heap at the 230k-node scale — while `layouts.ts` only ever calls
 * the six members below. Backing them with flat arrays makes every layout
 * switch start in O(1) memory and zero build time.
 *
 * Node keys are stringified indices ("0".."N-1"): layouts treat keys as
 * opaque identifiers and read human labels from the `label` attribute.
 */
export interface LayoutGraphApi {
  readonly order: number
  forEachNode(callback: (node: string, attributes: Record<string, unknown>) => void): void
  forEachNeighbor(node: unknown, callback: (neighbor: string) => void): void
  getNodeAttribute(node: unknown, name: string): unknown
  setNodeAttribute(node: unknown, name: string, value: unknown): void
  degree(node: unknown): number
}

export interface LayoutTopology {
  /** Node count. */
  count: number
  /** Display label per node index (drives importance tie-breaking). */
  labels: readonly string[]
  /** Node type per node index (drives type grouping layouts). */
  types: readonly string[]
  /** Visual radius per node index. */
  sizes: Float32Array
  /** CSR adjacency: neighbor list offsets, `count + 1` entries. */
  neighborOffsets: Uint32Array
  /** CSR adjacency: flat neighbor node indices. */
  neighborIndices: Uint32Array
}

export class LayoutGraph implements LayoutGraphApi {
  readonly order: number
  /** Interleaved x/y positions, written by layouts via `setNodeAttribute`. */
  readonly positions: Float32Array

  private readonly topology: LayoutTopology

  constructor(topology: LayoutTopology) {
    this.topology = topology
    this.order = topology.count
    this.positions = new Float32Array(topology.count * 2)
  }

  forEachNode(callback: (node: string, attributes: Record<string, unknown>) => void): void {
    for (let index = 0; index < this.order; index += 1) {
      callback(String(index), {
        nodeType: this.topology.types[index],
        label: this.topology.labels[index],
        size: this.topology.sizes[index],
      })
    }
  }

  forEachNeighbor(node: unknown, callback: (neighbor: string) => void): void {
    const index = Number(node)
    const { neighborOffsets, neighborIndices } = this.topology
    const end = neighborOffsets[index + 1] ?? 0
    for (let cursor = neighborOffsets[index] ?? 0; cursor < end; cursor += 1) {
      callback(String(neighborIndices[cursor]))
    }
  }

  getNodeAttribute(node: unknown, name: string): unknown {
    const index = Number(node)
    if (name === 'x') return this.positions[index * 2]
    if (name === 'y') return this.positions[index * 2 + 1]
    if (name === 'label') return this.topology.labels[index]
    if (name === 'nodeType') return this.topology.types[index]
    if (name === 'size') return this.topology.sizes[index]
    return undefined
  }

  setNodeAttribute(node: unknown, name: string, value: unknown): void {
    const index = Number(node)
    if (typeof value !== 'number') return
    if (name === 'x') this.positions[index * 2] = value
    else if (name === 'y') this.positions[index * 2 + 1] = value
  }

  degree(node: unknown): number {
    const index = Number(node)
    const { neighborOffsets } = this.topology
    return (neighborOffsets[index + 1] ?? 0) - (neighborOffsets[index] ?? 0)
  }
}
