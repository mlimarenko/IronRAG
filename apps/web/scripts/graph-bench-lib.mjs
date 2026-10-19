// Pure helpers behind the graph fixture generator and the graph bench driver.
//
// Everything here is deterministic and side-effect free so the bench itself
// is unit-testable: fixture topology sampling (power-law degrees), NDJSON
// frame chunking that mirrors the backend wire contract, and the metric
// summaries the bench prints. The Playwright driver (`graph-bench.mjs`) and
// the fixture CLI (`graph-fixture.mjs`) stay thin shells over this module.

/** Deterministic 32-bit PRNG (mulberry32). */
export function createRandom(seed) {
  let state = seed >>> 0
  return () => {
    state = (state + 0x6d2b79f5) >>> 0
    let t = state
    t = Math.imul(t ^ (t >>> 15), t | 1)
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61)
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296
  }
}

/** Deterministic UUID-shaped id for a fixture node index. */
export function fixtureUuid(index) {
  const hex = (value, width) => value.toString(16).padStart(width, '0')
  const a = hex(Math.imul(index + 1, 2654435761) >>> 0, 8)
  const b = hex(Math.imul(index + 7, 40503) & 0xffff, 4)
  const c = hex(0x4000 | (index & 0x0fff), 4)
  const d = hex(0x8000 | ((index * 31) & 0x3fff), 4)
  const e = hex(index >>> 0, 12)
  return `${a}-${b}-${c}-${d}-${e}`
}

const NODE_TYPES = [
  ['concept', 30],
  ['person', 14],
  ['organization', 10],
  ['artifact', 12],
  ['process', 8],
  ['event', 7],
  ['location', 6],
  ['attribute', 6],
  ['natural', 3],
  ['entity', 4],
]

function pickWeighted(entries, roll) {
  const total = entries.reduce((sum, [, weight]) => sum + weight, 0)
  let remaining = roll * total
  for (const [value, weight] of entries) {
    remaining -= weight
    if (remaining <= 0) return value
  }
  return entries[entries.length - 1][0]
}

const PREDICATES = ['related_to', 'part_of', 'refers_to', 'derived_from', 'located_in']
const WORDS = [
  'alpha',
  'beta',
  'gamma',
  'delta',
  'sigma',
  'vector',
  'signal',
  'module',
  'anchor',
  'branch',
  'circuit',
  'domain',
  'engine',
  'filter',
  'gateway',
  'harbor',
  'index',
  'kernel',
  'lattice',
  'matrix',
  'node',
  'orbit',
  'prism',
  'quartz',
  'relay',
  'stream',
  'tensor',
  'utility',
  'vertex',
  'wave',
]

function fixtureLabel(index, random) {
  const first = WORDS[Math.floor(random() * WORDS.length)]
  const second = WORDS[Math.floor(random() * WORDS.length)]
  return `${first} ${second} ${index}`
}

/**
 * Build a synthetic topology with a power-law degree distribution.
 * Edge targets use preferential attachment (pick an endpoint of an existing
 * edge with high probability), so a few hub nodes accumulate very large
 * neighborhoods — the worst case for neighbor highlighting.
 *
 * `nodeCount` covers entities + documents; `edgeCount` covers entity-entity
 * relations + document links, mirroring how the product counts them.
 */
export function buildFixtureTopology({ nodeCount, edgeCount, seed = 1337, documentShare = 0.05 }) {
  const random = createRandom(seed)
  const documentCount = Math.max(1, Math.round(nodeCount * documentShare))
  const entityCount = nodeCount - documentCount

  const entities = []
  for (let index = 0; index < entityCount; index += 1) {
    const type = pickWeighted(NODE_TYPES, random())
    entities.push({
      num: index,
      label: fixtureLabel(index, random),
      type,
      subType: random() < 0.2 ? `${type}_variant` : undefined,
      support: 1 + Math.floor(random() * 9),
      confidence: Math.round((0.5 + random() * 0.5) * 100) / 100,
      summary: random() < 0.15 ? `Synthetic summary for fixture node ${index}.` : undefined,
    })
  }

  const documents = []
  for (let index = 0; index < documentCount; index += 1) {
    const num = entityCount + index
    documents.push({
      num,
      title: `Fixture document ${index}`,
      fileName: `fixture-${index}.md`,
    })
  }

  const relationCount = Math.min(edgeCount, Math.max(0, Math.round(edgeCount * 0.7)))
  const linkCount = edgeCount - relationCount

  // Preferential attachment pool: node numbers repeated once per incident
  // edge. Sampling from the pool makes attachment probability proportional
  // to current degree (power-law hubs emerge naturally). A dedicated hub
  // tier on top pushes the maximum degree into the thousands — hubs are the
  // worst case for neighbor highlighting and must exist in every fixture.
  const hubTier = Math.max(1, Math.min(20, Math.floor(entityCount / 500)))
  const pool = []
  const relations = []
  const seen = new Set()
  const pickEntity = () => {
    const roll = random()
    if (roll < 0.05) return Math.floor(random() * hubTier)
    if (pool.length > 0 && roll < 0.75) {
      return pool[Math.floor(random() * pool.length)]
    }
    return Math.floor(random() * entityCount)
  }
  fillRelations({
    entityCount,
    nodeCount,
    pickEntity,
    pool,
    random,
    relationCount,
    relations,
    seen,
  })

  const documentLinks = []
  fillDocumentLinks({
    documentCount,
    documentLinks,
    entityCount,
    linkCount,
    nodeCount,
    pickEntity,
    pool,
    random,
  })

  return { entities, documents, relations, documentLinks }
}

/**
 * Undirected edge identity. Both endpoints in either order must collapse to
 * one key, or the fixture emits the same relation twice and the renderer's
 * neighbour counts stop matching the declared edge count.
 */
function undirectedEdgeKey(source, target, nodeCount) {
  return source < target ? source * nodeCount + target : target * nodeCount + source
}

/**
 * Draw entity-entity relations until the target count is met.
 *
 * The `guard` is not decoration: rejection sampling on a dense fixture can run
 * out of unseen pairs, and without a ceiling the loop never terminates.
 */
function fillRelations({
  entityCount,
  nodeCount,
  pickEntity,
  pool,
  random,
  relationCount,
  relations,
  seen,
}) {
  let guard = 0
  while (relations.length < relationCount && guard < relationCount * 20) {
    guard += 1
    const source = Math.floor(random() * entityCount)
    const target = pickEntity()
    if (source === target) continue
    const key = undirectedEdgeKey(source, target, nodeCount)
    if (seen.has(key)) continue
    seen.add(key)
    const predicate = PREDICATES[Math.floor(random() * PREDICATES.length)]
    relations.push([source, target, predicate, 1 + Math.floor(random() * 5)])
    pool.push(source, target)
  }
}

/** Draw document-to-entity links until the target count is met. */
function fillDocumentLinks({
  documentCount,
  documentLinks,
  entityCount,
  linkCount,
  nodeCount,
  pickEntity,
  pool,
  random,
}) {
  const linkSeen = new Set()
  let guard = 0
  while (documentLinks.length < linkCount && guard < linkCount * 20) {
    guard += 1
    const doc = entityCount + Math.floor(random() * documentCount)
    const target = pickEntity()
    const key = doc * nodeCount + target
    if (linkSeen.has(key)) continue
    linkSeen.add(key)
    documentLinks.push([doc, target, 'supports', 1 + Math.floor(random() * 3)])
    pool.push(target)
  }
}

/**
 * Serialize a fixture topology into NDJSON frames matching the backend
 * graph-stream wire contract (`meta` → `id_map` → `docs` → `nodes` →
 * `edges` → `doc_links` → `end`), chunked like the server chunks rows.
 */
export function topologyToNdjsonFrames(topology, { rowsPerFrame = 4000 } = {}) {
  const { entities, documents, relations, documentLinks } = topology
  const frames = []
  const nodeCount = entities.length + documents.length
  const edgeCount = relations.length + documentLinks.length
  frames.push({ s: 'meta', node_count: nodeCount, edge_count: edgeCount })

  const idEntries = []
  for (const entity of entities) idEntries.push([fixtureUuid(entity.num), entity.num])
  for (const doc of documents) idEntries.push([fixtureUuid(doc.num), doc.num])
  for (let start = 0; start < idEntries.length; start += rowsPerFrame * 2) {
    frames.push({
      s: 'id_map',
      m: Object.fromEntries(idEntries.slice(start, start + rowsPerFrame * 2)),
    })
  }

  const docRows = documents.map((doc) => ({ i: doc.num, t: doc.title, fn: doc.fileName }))
  for (let start = 0; start < docRows.length; start += rowsPerFrame) {
    frames.push({ s: 'docs', d: docRows.slice(start, start + rowsPerFrame) })
  }

  const nodeRows = entities.map((entity) => ({
    i: entity.num,
    l: entity.label,
    t: entity.type,
    ...(entity.subType ? { ts: entity.subType } : {}),
    s: entity.support,
    c: entity.confidence,
    es: 'active',
    ...(entity.summary ? { sm: entity.summary } : {}),
  }))
  for (let start = 0; start < nodeRows.length; start += rowsPerFrame) {
    frames.push({ s: 'nodes', d: nodeRows.slice(start, start + rowsPerFrame) })
  }

  for (let start = 0; start < relations.length; start += rowsPerFrame * 2) {
    frames.push({ s: 'edges', d: relations.slice(start, start + rowsPerFrame * 2) })
  }
  for (let start = 0; start < documentLinks.length; start += rowsPerFrame * 2) {
    frames.push({ s: 'doc_links', d: documentLinks.slice(start, start + rowsPerFrame * 2) })
  }

  frames.push({ s: 'end' })
  return frames
}

/** Degree histogram sanity metric: max degree and share of degree-0 nodes. */
export function degreeStats(topology) {
  const degrees = new Map()
  const bump = (num) => degrees.set(num, (degrees.get(num) ?? 0) + 1)
  for (const [source, target] of topology.relations) {
    bump(source)
    bump(target)
  }
  for (const [doc, target] of topology.documentLinks) {
    bump(doc)
    bump(target)
  }
  const nodeCount = topology.entities.length + topology.documents.length
  let maxDegree = 0
  let maxNode = -1
  for (const [num, degree] of degrees) {
    if (degree > maxDegree) {
      maxDegree = degree
      maxNode = num
    }
  }
  return {
    maxDegree,
    maxDegreeNode: maxNode,
    isolatedShare: (nodeCount - degrees.size) / Math.max(1, nodeCount),
  }
}

/** Quantile over a plain number array (nearest-rank, sorted copy). */
export function quantile(values, percentile) {
  if (values.length === 0) return 0
  const sorted = [...values].sort((left, right) => left - right)
  const index = Math.min(sorted.length - 1, Math.floor(percentile * sorted.length))
  return sorted[index]
}

/** Summarize frame intervals + long tasks captured by the page profiler. */
export function summarizeProfile({ frames, longtasks }) {
  const total = frames.reduce((sum, value) => sum + value, 0)
  return {
    frames: frames.length,
    fps: frames.length > 0 && total > 0 ? Math.round(1000 / (total / frames.length)) : 0,
    p50: Number(quantile(frames, 0.5).toFixed(1)),
    p95: Number(quantile(frames, 0.95).toFixed(1)),
    max: Number(quantile(frames, 1).toFixed(1)),
    dropped: frames.filter((interval) => interval > 32).length,
    longtasks: longtasks.length,
    longtaskMs: Number(longtasks.reduce((sum, value) => sum + value, 0).toFixed(0)),
    longtaskMax: Number(quantile(longtasks, 1).toFixed(0)),
  }
}

/** Render one bench scenario row for the report table. */
export function formatScenarioRow(label, profile) {
  return (
    `${label.padEnd(22)} fps=${String(profile.fps).padStart(3)} ` +
    `p50=${String(profile.p50).padStart(6)}ms p95=${String(profile.p95).padStart(7)}ms ` +
    `max=${String(profile.max).padStart(7)}ms dropped=${String(profile.dropped).padStart(3)} ` +
    `longtasks=${String(profile.longtasks).padStart(2)}/${String(profile.longtaskMs).padStart(5)}ms ` +
    `worst=${String(profile.longtaskMax).padStart(5)}ms`
  )
}
