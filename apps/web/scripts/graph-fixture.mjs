#!/usr/bin/env node
// Synthetic NDJSON graph-topology fixture generator for the render bench.
//
// Reproduces the backend graph-stream wire contract (meta → id_map → docs →
// nodes → edges → doc_links → end) at controlled scales with a power-law
// degree distribution, so the render pipeline can be measured without any
// backend or real corpus. See `graph-bench-lib.mjs` for the sampling logic.
//
//   node scripts/graph-fixture.mjs --scale 100k --out /tmp/graph-fixtures
//   node scripts/graph-fixture.mjs --nodes 50000 --edges 80000 --out DIR
//
// Scales: 10k (10k nodes / 1k edges), 100k (97k / 120k), 230k (229k / 652k).
import { mkdir, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import { buildFixtureTopology, degreeStats, topologyToNdjsonFrames } from './graph-bench-lib.mjs'

export const FIXTURE_SCALES = {
  '10k': { nodeCount: 10_000, edgeCount: 1_000 },
  '100k': { nodeCount: 97_000, edgeCount: 120_000 },
  '230k': { nodeCount: 229_000, edgeCount: 652_000 },
}

function parseArgs(argv) {
  const args = { out: join(tmpdir(), 'ironrag-graph-fixtures'), scales: [] }
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index]
    if (arg === '--out') args.out = argv[++index]
    else if (arg === '--scale') args.scales.push(argv[++index])
    else if (arg === '--nodes') args.nodes = Number(argv[++index])
    else if (arg === '--edges') args.edges = Number(argv[++index])
    else throw new Error(`unknown argument: ${arg}`)
  }
  if (args.scales.length === 0 && !args.nodes) args.scales = Object.keys(FIXTURE_SCALES)
  return args
}

async function writeFixture(outDir, name, { nodeCount, edgeCount }) {
  const startedAt = performance.now()
  const topology = buildFixtureTopology({ nodeCount, edgeCount })
  const frames = topologyToNdjsonFrames(topology)
  const body = frames.map((frame) => JSON.stringify(frame)).join('\n') + '\n'
  const path = join(outDir, `graph-${name}.ndjson`)
  await writeFile(path, body)
  const stats = degreeStats(topology)
  console.log(
    `${path}: ${nodeCount} nodes, ${edgeCount} edges, ` +
      `${(body.length / 1024 / 1024).toFixed(1)} MiB, ` +
      `max degree ${stats.maxDegree}, isolated ${(stats.isolatedShare * 100).toFixed(0)}%, ` +
      `${Math.round(performance.now() - startedAt)}ms`,
  )
  return path
}

const isDirectRun = process.argv[1]?.endsWith('graph-fixture.mjs')
if (isDirectRun) {
  const args = parseArgs(process.argv.slice(2))
  await mkdir(args.out, { recursive: true })
  if (args.nodes) {
    await writeFixture(args.out, `${args.nodes}n-${args.edges ?? 0}e`, {
      nodeCount: args.nodes,
      edgeCount: args.edges ?? 0,
    })
  }
  for (const scale of args.scales) {
    const preset = FIXTURE_SCALES[scale]
    if (!preset) throw new Error(`unknown scale "${scale}" (known: ${Object.keys(FIXTURE_SCALES)})`)
    await writeFixture(args.out, scale, preset)
  }
}
