#!/usr/bin/env node
// Graph render bench: measures the graph view against synthetic NDJSON
// fixtures with a fully mocked API, so numbers depend only on the render
// pipeline (no backend, no corpus).
//
// Metrics per run:
//   - firstFrameMs:      NDJSON stream fully received -> first painted graph
//   - layoutSettledMs:   stream received -> final layout positions on screen
//   - layout switches:   click -> busy indicator cleared (per target layout)
//   - pan / wheel-zoom / hover-sweep frame profiles (p50/p95/max, long tasks)
//   - hover + click-select latency (when the renderer exposes debug hooks)
//
// Usage:
//   npm run build   (dist/ must exist — the bench measures the prod bundle)
//   node scripts/graph-fixture.mjs --out /tmp/ironrag-graph-fixtures
//   node scripts/graph-bench.mjs --scale 100k --label baseline
//
// Options: --scale 10k|100k|230k  --fixtures DIR  --label NAME  --out DIR
//          --port N  --headed  --switches force,hubs,circlepack
import { spawn } from 'node:child_process'
import { copyFile, mkdir, stat, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

import { chromium } from '@playwright/test'

import { formatScenarioRow, summarizeProfile } from './graph-bench-lib.mjs'

const here = dirname(fileURLToPath(import.meta.url))
const webRoot = resolve(here, '..')
// Absolute paths on purpose: `npx vite` resolves both `npx` and `vite` through
// PATH, so whatever directory happens to sit first there decides what runs.
const VITE_CLI = join(webRoot, 'node_modules', 'vite', 'bin', 'vite.js')

const WORKSPACE_ID = 'bench-workspace'
const LIBRARY_ID = 'bench-library'
// `--use-angle=gl-egl` picks the real GPU in headless mode; the previous
// `--use-gl=egl` silently fell back to SwiftShader software rendering and
// every frame-time number measured the CPU rasterizer instead of the GPU.
// Override with GPU_ARGS="..." (e.g. "--use-angle=vulkan ..." for NVIDIA).
const GPU_ARGS = (
  process.env.GPU_ARGS ?? '--use-angle=gl-egl --enable-gpu --ignore-gpu-blocklist --no-sandbox'
).split(' ')

function parseArgs(argv) {
  const args = {
    scale: '100k',
    fixtures: join(tmpdir(), 'ironrag-graph-fixtures'),
    label: 'run',
    out: join(tmpdir(), 'ironrag-graph-bench'),
    port: 4181,
    headed: false,
    switches: ['hubs', 'circlepack', 'force'],
    settleTimeoutMs: 240_000,
  }
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index]
    if (arg === '--scale') args.scale = argv[++index]
    else if (arg === '--fixtures') args.fixtures = argv[++index]
    else if (arg === '--label') args.label = argv[++index]
    else if (arg === '--out') args.out = argv[++index]
    else if (arg === '--port') args.port = Number(argv[++index])
    else if (arg === '--headed') args.headed = true
    else if (arg === '--switches') args.switches = argv[++index].split(',').filter(Boolean)
    else if (arg === '--settle-timeout') args.settleTimeoutMs = Number(argv[++index]) * 1000
    else throw new Error(`unknown argument: ${arg}`)
  }
  return args
}

// Injected before any page script: frame/long-task profiler with phases, a
// rAF poller for the first painted graph frame, and a busy-indicator log.
const PAGE_INSTRUMENTATION = `
(() => {
  window.__IRONRAG_GRAPH_BENCH__ = true;
  const M = (window.__graphBench = {
    dataEndMs: null, canvasSeenMs: null, overlayClearedMs: null,
    lastLongtaskEndMs: 0, busyLog: [], phase: null, frames: [], longtasks: [], last: 0,
  });
  try {
    new PerformanceObserver((list) => {
      for (const entry of list.getEntries()) {
        if (entry.name.includes('/knowledge/libraries/') && entry.name.endsWith('/graph')) {
          M.dataEndMs = entry.responseEnd;
        }
      }
    }).observe({ type: 'resource', buffered: true });
  } catch { /* resource timing unavailable */ }
  try {
    new PerformanceObserver((list) => {
      for (const entry of list.getEntries()) {
        M.lastLongtaskEndMs = Math.max(M.lastLongtaskEndMs, entry.startTime + entry.duration);
        if (M.phase) M.longtasks.push(entry.duration);
      }
    }).observe({ entryTypes: ['longtask'] });
  } catch { /* longtask observer unavailable */ }
  const raf = window.requestAnimationFrame.bind(window);
  const poll = (t) => {
    if (M.phase) {
      if (M.last) M.frames.push(t - M.last);
      M.last = t;
    } else {
      M.last = 0;
    }
    const canvas = document.querySelector('.graph-canvas canvas');
    const overlay = document.querySelector('.graph-canvas__preparing');
    const busy = document.querySelector('.graph-layout-busy');
    if (canvas && M.canvasSeenMs == null) M.canvasSeenMs = t;
    if (canvas && !overlay && M.canvasSeenMs != null && M.overlayClearedMs == null) {
      M.overlayClearedMs = t;
    }
    const lastBusy = M.busyLog[M.busyLog.length - 1];
    const busyNow = Boolean(busy);
    if (!lastBusy || lastBusy.busy !== busyNow) M.busyLog.push({ busy: busyNow, at: t });
    raf(poll);
  };
  raf(poll);
  window.__perfStart = () => { M.phase = true; M.frames = []; M.longtasks = []; M.last = 0; };
  window.__perfStop = () => {
    M.phase = null;
    return { frames: M.frames.slice(), longtasks: M.longtasks.slice() };
  };
})();
`

function sessionPayload() {
  return {
    bootstrapStatus: { setupRequired: false },
    locale: 'en',
    me: {
      principal: { displayLabel: 'Bench User', id: 'principal-bench' },
      user: { displayName: 'Bench User', login: 'bench' },
    },
    message: null,
    mode: 'authenticated',
    session: { expiresAt: new Date(Date.now() + 86_400_000).toISOString(), id: 'session-bench' },
    shellBootstrap: {
      libraries: [
        {
          id: LIBRARY_ID,
          ingestionReady: true,
          missingBindingPurposes: [],
          name: 'Bench Library',
          slug: 'bench-library',
          workspaceId: WORKSPACE_ID,
        },
      ],
      workspaces: [{ id: WORKSPACE_ID, name: 'Bench Workspace', slug: 'bench' }],
    },
  }
}

function summaryPayload(meta) {
  return {
    libraryId: LIBRARY_ID,
    nodeCount: meta.nodeCount,
    edgeCount: meta.edgeCount,
    graphReadyDocumentCount: 1,
    graphSparseDocumentCount: 0,
    typedFactDocumentCount: 0,
    documentCountsByReadiness: {},
    latestGeneration: null,
    updatedAt: new Date().toISOString(),
  }
}

function entityPayload(entityId) {
  return {
    entity: {
      entityId,
      entityType: 'concept',
      supportCount: 3,
      entityState: 'active',
      aliases: [],
      summary: null,
      confidence: 0.9,
    },
    supportingEvidence: [],
  }
}

async function installApiMocks(page, meta) {
  await page.route('**/v1/**', async (route) => {
    const request = route.request()
    const url = new URL(request.url())
    const path = url.pathname
    if (path === '/v1/iam/context') {
      return route.fulfill({ json: sessionPayload() })
    }
    if (/^\/v1\/knowledge\/libraries\/[^/]+\/graph$/.test(path)) {
      return route.continue({ url: `${url.origin}/bench/graph.ndjson` })
    }
    if (/^\/v1\/knowledge\/libraries\/[^/]+\/summary$/.test(path)) {
      return route.fulfill({ json: summaryPayload(meta) })
    }
    const entityMatch = /^\/v1\/knowledge\/libraries\/[^/]+\/entities\/([^/]+)$/.exec(path)
    if (entityMatch) {
      return route.fulfill({ json: entityPayload(entityMatch[1]) })
    }
    return route.fulfill({ json: {} })
  })
}

function startPreviewServer(port) {
  // Own process group so shutdown kills the whole npx -> vite tree; a leaked
  // preview server makes the next bench run silently reuse a stale build.
  const child = spawn(
    process.execPath,
    [VITE_CLI, 'preview', '--host', '127.0.0.1', '--port', String(port), '--strictPort'],
    { cwd: webRoot, stdio: ['ignore', 'pipe', 'pipe'], detached: true },
  )
  child.stderr.on('data', (chunk) => {
    const text = String(chunk)
    process.stderr.write(`[preview] ${text}`)
    if (text.includes('is already in use')) {
      console.error('preview port is occupied by a stale server; kill it and retry')
      process.exit(2)
    }
  })
  return {
    stop() {
      try {
        process.kill(-child.pid, 'SIGTERM')
      } catch {
        child.kill('SIGTERM')
      }
    },
  }
}

async function waitForServer(baseUrl, timeoutMs = 20_000) {
  const deadline = Date.now() + timeoutMs
  while (Date.now() < deadline) {
    try {
      const response = await fetch(baseUrl)
      if (response.ok) return
    } catch {
      /* not up yet */
    }
    await new Promise((resolveSleep) => setTimeout(resolveSleep, 250))
  }
  throw new Error(`preview server did not start at ${baseUrl}`)
}

const sleep = (ms) => new Promise((resolveSleep) => setTimeout(resolveSleep, ms))

async function readBench(page) {
  return page.evaluate(() => {
    const M = window.__graphBench
    return {
      dataEndMs: M.dataEndMs,
      canvasSeenMs: M.canvasSeenMs,
      overlayClearedMs: M.overlayClearedMs,
      lastLongtaskEndMs: M.lastLongtaskEndMs,
      busyLog: M.busyLog.slice(),
      now: performance.now(),
      marks: performance
        .getEntriesByType('mark')
        .filter((mark) => mark.name.startsWith('graph:'))
        .map((mark) => ({ name: mark.name, at: mark.startTime })),
    }
  })
}

function markAt(bench, name) {
  const marks = bench.marks.filter((mark) => mark.name === name)
  return marks.length > 0 ? marks[marks.length - 1].at : null
}

/** First painted graph frame: explicit renderer mark, else overlay clearance. */
function resolveFirstFrame(bench) {
  return markAt(bench, 'graph:first-frame') ?? bench.overlayClearedMs
}

async function waitForFirstFrame(page, timeoutMs) {
  const deadline = Date.now() + timeoutMs
  while (Date.now() < deadline) {
    const bench = await readBench(page)
    const firstFrame = resolveFirstFrame(bench)
    if (bench.dataEndMs != null && firstFrame != null && firstFrame > 0) return bench
    await sleep(100)
  }
  throw new Error('graph first frame did not arrive in time')
}

/**
 * Initial layout settle: the renderer's explicit mark when present,
 * otherwise "no long task for 4 s" (the legacy renderer applies worker
 * positions in main-thread bursts, so quiet means settled).
 */
async function waitForSettle(page, timeoutMs) {
  const deadline = Date.now() + timeoutMs
  while (Date.now() < deadline) {
    const bench = await readBench(page)
    const marked = markAt(bench, 'graph:layout-settled')
    if (marked != null) return { settledMs: marked, timedOut: false }
    // The quiet heuristic (no long task for 4 s) exists only for renderers
    // that never emit marks (the legacy pipeline applied worker layouts in
    // main-thread bursts). A mark-emitting renderer settles exactly at its
    // mark: worker-side computation is silent on the main thread, so quiet
    // does NOT mean settled there.
    const emitsMarks = markAt(bench, 'graph:first-frame') != null
    if (
      !emitsMarks &&
      bench.now - Math.max(bench.lastLongtaskEndMs, bench.overlayClearedMs ?? 0) > 4_000
    ) {
      return {
        settledMs: Math.max(bench.lastLongtaskEndMs, bench.overlayClearedMs ?? 0),
        timedOut: false,
      }
    }
    await sleep(500)
  }
  const bench = await readBench(page)
  return { settledMs: bench.now, timedOut: true }
}

async function measurePhase(page, label, interaction, settleMs = 400) {
  await page.evaluate(() => window.__perfStart())
  await interaction()
  await sleep(settleMs)
  const raw = await page.evaluate(() => window.__perfStop())
  const profile = summarizeProfile(raw)
  console.log(formatScenarioRow(label, profile))
  return profile
}

async function measureLayoutSwitch(page, layoutId, timeoutMs) {
  const picker = page.locator('.graph-layout-picker select').first()
  if ((await picker.count()) === 0) return { error: 'layout picker not found' }
  if ((await picker.inputValue()) === layoutId) {
    return { error: 'already active' }
  }

  const clickAt = await page.evaluate(() => performance.now())
  await page.evaluate(() => window.__perfStart())
  await picker.selectOption(layoutId)

  const deadline = Date.now() + timeoutMs
  let settledAt = null
  while (Date.now() < deadline) {
    const bench = await readBench(page)
    const marked = bench.marks
      .filter((mark) => mark.name === 'graph:layout-switch-settled' && mark.at > clickAt)
      .map((mark) => mark.at)
    if (marked.length > 0) {
      settledAt = marked[marked.length - 1]
      break
    }
    const cleared = bench.busyLog.find((entry) => !entry.busy && entry.at > clickAt + 50)
    const appeared = bench.busyLog.some((entry) => entry.busy && entry.at > clickAt)
    if (appeared && cleared) {
      settledAt = cleared.at
      break
    }
    await sleep(150)
  }
  const raw = await page.evaluate(() => window.__perfStop())
  const profile = summarizeProfile(raw)
  if (settledAt == null) return { error: `timeout after ${timeoutMs}ms`, profile }
  return { switchMs: Math.round(settledAt - clickAt), profile }
}

async function runScenarios(page, args, results) {
  const box = await page.locator('.graph-canvas canvas').first().boundingBox()
  if (!box) throw new Error('graph canvas has no bounding box')
  const centerX = box.x + box.width * 0.55
  const centerY = box.y + box.height * 0.5

  results.pan = await measurePhase(page, 'pan', async () => {
    await page.mouse.move(centerX, centerY)
    await page.mouse.down()
    for (let step = 0; step < 24; step += 1) {
      await page.mouse.move(centerX - step * 14, centerY - step * 7)
      await sleep(16)
    }
    await page.mouse.up()
  })

  results.wheelZoom = await measurePhase(page, 'wheel-zoom', async () => {
    for (let step = 0; step < 10; step += 1) {
      await page.mouse.move(centerX, centerY)
      await page.mouse.wheel(0, step % 2 ? 260 : -260)
      await sleep(110)
    }
  })

  results.hoverSweep = await measurePhase(page, 'hover-sweep', async () => {
    for (let step = 0; step < 36; step += 1) {
      await page.mouse.move(centerX - 160 + step * 9, centerY + Math.sin(step / 3) * 60)
      await sleep(33)
    }
  })

  const debugInfo = await page.evaluate(() => {
    const debug = window.__graphDebug
    if (!debug) return null
    return {
      hubScreen: debug.screenPositionOf?.('hub') ?? null,
      drawnNodes: debug.drawnNodeCount?.() ?? null,
      drawnEdges: debug.drawnEdgeCount?.() ?? null,
    }
  })
  results.drawn = debugInfo ? { nodes: debugInfo.drawnNodes, edges: debugInfo.drawnEdges } : null

  if (debugInfo?.hubScreen) {
    results.hoverHub = await measurePhase(page, 'hover-hub', async () => {
      await page.mouse.move(box.x + 4, box.y + 4)
      await sleep(120)
      await page.mouse.move(debugInfo.hubScreen.x, debugInfo.hubScreen.y)
      await sleep(400)
    })
    results.hoverLatency = await page.evaluate(() => window.__graphDebug?.hoverStats?.() ?? null)
  }

  const clickTarget = debugInfo?.hubScreen ?? { x: centerX, y: centerY }
  const clickStart = await page.evaluate(() => performance.now())
  await page.mouse.click(clickTarget.x, clickTarget.y)
  const inspectorVisible = await page
    .waitForSelector('.graph-inspector', { timeout: 15_000 })
    .then(() => true)
    .catch(() => false)
  if (inspectorVisible) {
    const clickEnd = await page.evaluate(() => performance.now())
    results.clickToInspectorMs = Math.round(clickEnd - clickStart)
    await page.keyboard.press('Escape')
    await sleep(300)
  } else {
    results.clickToInspectorMs = null
  }

  results.layoutSwitches = {}
  for (const layout of args.switches) {
    const outcome = await measureLayoutSwitch(page, layout, args.settleTimeoutMs)
    results.layoutSwitches[layout] = outcome
    const label = `switch->${layout}`
    if (outcome.switchMs != null) console.log(`${label.padEnd(22)} ${outcome.switchMs}ms`)
    else console.log(`${label.padEnd(22)} ${outcome.error}`)
    await sleep(500)
  }

  results.heapMb = await page.evaluate(() =>
    performance.memory ? Math.round(performance.memory.usedJSHeapSize / 1048576) : null,
  )
}

async function main() {
  const args = parseArgs(process.argv.slice(2))
  const fixturePath = join(args.fixtures, `graph-${args.scale}.ndjson`)
  await stat(fixturePath).catch(() => {
    throw new Error(`fixture missing: ${fixturePath} (run scripts/graph-fixture.mjs first)`)
  })
  const distDir = join(webRoot, 'dist')
  await stat(join(distDir, 'index.html')).catch(() => {
    throw new Error('dist/index.html missing (run npm run build first)')
  })

  const meta = await (async () => {
    const { createReadStream } = await import('node:fs')
    const stream = createReadStream(fixturePath, { end: 4096 })
    let head = ''
    for await (const chunk of stream) {
      head += chunk.toString('utf8')
      if (head.includes('\n')) break
    }
    const metaFrame = JSON.parse(head.slice(0, head.indexOf('\n')))
    return { nodeCount: metaFrame.node_count, edgeCount: metaFrame.edge_count }
  })()

  await mkdir(join(distDir, 'bench'), { recursive: true })
  await copyFile(fixturePath, join(distDir, 'bench', 'graph.ndjson'))
  await mkdir(args.out, { recursive: true })

  const baseUrl = `http://127.0.0.1:${args.port}`
  const server = startPreviewServer(args.port)
  for (const signal of ['SIGTERM', 'SIGINT']) {
    process.on(signal, () => {
      server.stop()
      process.exit(1)
    })
  }
  const results = {
    label: args.label,
    scale: args.scale,
    meta,
    startedAt: new Date().toISOString(),
  }

  let browser = null
  try {
    await waitForServer(baseUrl)
    browser = await chromium.launch({ headless: !args.headed, args: GPU_ARGS })
    const context = await browser.newContext({ viewport: { width: 1680, height: 1000 } })
    await context.addInitScript(PAGE_INSTRUMENTATION)
    await context.addInitScript(
      ({ workspaceId, libraryId }) => {
        localStorage.setItem('ironrag.v1.active-workspace', workspaceId)
        localStorage.setItem('ironrag.v1.active-library', libraryId)
        localStorage.setItem('ironrag.v1.locale', 'en')
      },
      { workspaceId: WORKSPACE_ID, libraryId: LIBRARY_ID },
    )
    const page = await context.newPage()
    page.on('pageerror', (error) => console.log('  [pageerror]', error.message))
    await installApiMocks(page, meta)

    console.log(
      `\n=== graph bench: scale=${args.scale} label=${args.label} ` +
        `(${meta.nodeCount} nodes / ${meta.edgeCount} edges) ===`,
    )
    await page.goto(`${baseUrl}/graph`, { waitUntil: 'domcontentloaded' })
    results.glRenderer = await page.evaluate(() => {
      const probe = document.createElement('canvas').getContext('webgl2')
      if (!probe) return 'no-webgl2'
      const ext = probe.getExtension('WEBGL_debug_renderer_info')
      return String(
        ext ? probe.getParameter(ext.UNMASKED_RENDERER_WEBGL) : probe.getParameter(probe.RENDERER),
      )
    })
    console.log(`gl: ${results.glRenderer}`)

    const bench = await waitForFirstFrame(page, 120_000)
    const firstFrame = resolveFirstFrame(bench)
    results.firstFrameMs = Math.round(firstFrame - bench.dataEndMs)
    console.log(`first-frame           ${results.firstFrameMs}ms after stream end`)

    const settle = await waitForSettle(page, args.settleTimeoutMs)
    results.layoutSettledMs = settle.timedOut
      ? null
      : Math.round(settle.settledMs - bench.dataEndMs)
    results.layoutSettleTimedOut = settle.timedOut
    const settleSummary = settle.timedOut
      ? `TIMEOUT >${args.settleTimeoutMs}ms`
      : `${results.layoutSettledMs}ms after stream end`
    console.log(`layout-settled        ${settleSummary}`)

    await runScenarios(page, args, results)

    await page.evaluate(() => performance.clearMarks())
    await page.reload({ waitUntil: 'domcontentloaded' })
    const reopenBench = await waitForFirstFrame(page, 120_000).catch(() => null)
    if (reopenBench) {
      const reopenFirst = resolveFirstFrame(reopenBench)
      results.reopenFirstFrameMs = Math.round(reopenFirst - reopenBench.dataEndMs)
      const reopenSettle = await waitForSettle(page, 30_000)
      results.reopenSettledMs = reopenSettle.timedOut
        ? null
        : Math.round(reopenSettle.settledMs - reopenBench.dataEndMs)
      console.log(
        `reopen                first-frame ${results.reopenFirstFrameMs}ms, ` +
          `settled ${results.reopenSettledMs ?? '>30000'}ms`,
      )
    }
  } finally {
    await browser?.close().catch(() => undefined)
    server.stop()
  }

  const outPath = join(args.out, `${args.label}-${args.scale}.json`)
  await writeFile(outPath, JSON.stringify(results, null, 2))
  console.log(`\nresults: ${outPath}`)
}

try {
  await main()
  process.exit(0)
} catch (error) {
  console.error(error)
  process.exit(1)
}
