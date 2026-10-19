#!/usr/bin/env node
// Visual QA screenshots for the graph view against a fixture, using the same
// mocked-API environment as graph-bench.mjs. Captures the settled force
// layout, hub hover highlight, node selection with the inspector, a zoomed-in
// label view, a geometric layout, and a narrow-viewport shot.
//
//   node scripts/graph-shots.mjs --scale 10k --out /tmp/graph-shots
import { spawn } from 'node:child_process'
import { copyFile, mkdir, stat } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

import { chromium } from '@playwright/test'

const here = dirname(fileURLToPath(import.meta.url))
const webRoot = resolve(here, '..')
// Absolute paths on purpose: `npx vite` resolves both `npx` and `vite` through
// PATH, so whatever directory happens to sit first there decides what runs.
const VITE_CLI = join(webRoot, 'node_modules', 'vite', 'bin', 'vite.js')
const WORKSPACE_ID = 'bench-workspace'
const LIBRARY_ID = 'bench-library'

function parseArgs(argv) {
  const args = {
    scale: '10k',
    fixtures: join(tmpdir(), 'ironrag-graph-fixtures'),
    out: join(tmpdir(), 'ironrag-graph-shots'),
    port: 4186,
  }
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index]
    if (arg === '--scale') args.scale = argv[++index]
    else if (arg === '--fixtures') args.fixtures = argv[++index]
    else if (arg === '--out') args.out = argv[++index]
    else if (arg === '--port') args.port = Number(argv[++index])
    else throw new Error(`unknown argument: ${arg}`)
  }
  return args
}

const sleep = (ms) => new Promise((resolveSleep) => setTimeout(resolveSleep, ms))

async function installMocks(page, meta) {
  await page.route('**/v1/**', async (route) => {
    const url = new URL(route.request().url())
    if (url.pathname === '/v1/iam/context') {
      return route.fulfill({
        json: {
          bootstrapStatus: { setupRequired: false },
          locale: 'en',
          me: {
            principal: { displayLabel: 'QA', id: 'principal-qa' },
            user: { displayName: 'QA', login: 'qa' },
          },
          message: null,
          mode: 'authenticated',
          session: { expiresAt: new Date(Date.now() + 86_400_000).toISOString(), id: 's-qa' },
          shellBootstrap: {
            libraries: [
              {
                id: LIBRARY_ID,
                ingestionReady: true,
                missingBindingPurposes: [],
                name: 'QA Library',
                slug: 'qa-library',
                workspaceId: WORKSPACE_ID,
              },
            ],
            workspaces: [{ id: WORKSPACE_ID, name: 'QA Workspace', slug: 'qa' }],
          },
        },
      })
    }
    if (url.pathname.endsWith('/graph') && url.pathname.includes('/knowledge/libraries/')) {
      return route.continue({ url: `${url.origin}/bench/graph.ndjson` })
    }
    if (url.pathname.endsWith('/summary')) {
      return route.fulfill({
        json: {
          libraryId: LIBRARY_ID,
          nodeCount: meta.nodeCount,
          edgeCount: meta.edgeCount,
          graphReadyDocumentCount: 1,
          graphSparseDocumentCount: 0,
          typedFactDocumentCount: 0,
          documentCountsByReadiness: {},
          latestGeneration: null,
          updatedAt: new Date().toISOString(),
        },
      })
    }
    const entityMatch = /\/entities\/([^/]+)$/.exec(url.pathname)
    if (entityMatch) {
      return route.fulfill({
        json: {
          entity: {
            entityId: entityMatch[1],
            entityType: 'concept',
            supportCount: 3,
            entityState: 'active',
            aliases: [],
            summary: null,
            confidence: 0.9,
          },
          supportingEvidence: [],
        },
      })
    }
    return route.fulfill({ json: {} })
  })
}

async function main() {
  const args = parseArgs(process.argv.slice(2))
  const fixturePath = join(args.fixtures, `graph-${args.scale}.ndjson`)
  await stat(fixturePath)
  await mkdir(join(webRoot, 'dist', 'bench'), { recursive: true })
  await copyFile(fixturePath, join(webRoot, 'dist', 'bench', 'graph.ndjson'))
  await mkdir(args.out, { recursive: true })
  const metaLine = (await import('node:fs')).readFileSync(fixturePath, 'utf8').split('\n', 1)[0]
  const metaFrame = JSON.parse(metaLine)
  const meta = { nodeCount: metaFrame.node_count, edgeCount: metaFrame.edge_count }

  const server = spawn(
    process.execPath,
    [VITE_CLI, 'preview', '--host', '127.0.0.1', '--port', String(args.port), '--strictPort'],
    { cwd: webRoot, stdio: 'ignore', detached: true },
  )
  const stopServer = () => {
    try {
      process.kill(-server.pid, 'SIGTERM')
    } catch {
      server.kill('SIGTERM')
    }
  }

  const browser = await chromium.launch({
    headless: true,
    args: ['--use-angle=gl-egl', '--enable-gpu', '--ignore-gpu-blocklist', '--no-sandbox'],
  })
  try {
    await sleep(1500)
    const context = await browser.newContext({ viewport: { width: 1680, height: 1000 } })
    await context.addInitScript(
      ({ workspaceId, libraryId }) => {
        window.__IRONRAG_GRAPH_BENCH__ = true
        localStorage.setItem('ironrag.v1.active-workspace', workspaceId)
        localStorage.setItem('ironrag.v1.active-library', libraryId)
        localStorage.setItem('ironrag.v1.locale', 'en')
      },
      { workspaceId: WORKSPACE_ID, libraryId: LIBRARY_ID },
    )
    const page = await context.newPage()
    page.on('pageerror', (error) => console.log('[pageerror]', error.message))
    await installMocks(page, meta)
    await page.goto(`http://127.0.0.1:${args.port}/graph`, { waitUntil: 'domcontentloaded' })
    await page.waitForSelector('.graph-canvas__gl', { timeout: 60_000 })
    await page
      .waitForFunction(() => performance.getEntriesByName('graph:layout-settled').length > 0, {
        timeout: 120_000,
      })
      .catch(() => console.log('WARN: layout settle mark not seen'))
    await sleep(800)
    const shot = (name) => page.screenshot({ path: join(args.out, `${args.scale}-${name}.png`) })
    await shot('01-settled-force')

    const hub = await page.evaluate(() => window.__graphDebug?.screenPositionOf('hub') ?? null)
    if (hub) {
      await page.mouse.move(hub.x, hub.y)
      await sleep(500)
      await shot('02-hover-hub')
      await page.mouse.click(hub.x, hub.y)
      await page.waitForSelector('.graph-inspector', { timeout: 10_000 }).catch(() => undefined)
      await sleep(400)
      await shot('03-selected-inspector')
      await page.keyboard.press('Escape')
      await sleep(400)
    }

    for (let step = 0; step < 6; step += 1) {
      await page.mouse.move(840, 500)
      await page.mouse.wheel(0, -400)
      await sleep(150)
    }
    await sleep(600)
    await shot('04-zoomed-labels')

    await page.locator('.graph-layout-picker select').selectOption('circlepack')
    await sleep(2500)
    await shot('05-circlepack')

    await page.setViewportSize({ width: 720, height: 900 })
    await sleep(1200)
    await shot('06-narrow-viewport')
    const narrowHub = await page.evaluate(
      () => window.__graphDebug?.screenPositionOf('hub') ?? null,
    )
    if (narrowHub) {
      await page.mouse.click(narrowHub.x, narrowHub.y)
      await page.waitForSelector('.graph-inspector', { timeout: 10_000 }).catch(() => undefined)
      await sleep(400)
      await shot('07-narrow-inspector')
      await page.setViewportSize({ width: 390, height: 844 })
      await sleep(600)
      await shot('08-phone-inspector')
    }
    console.log(`shots written to ${args.out}`)
  } finally {
    await browser.close().catch(() => undefined)
    stopServer()
  }
}

try {
  await main()
  process.exit(0)
} catch (error) {
  console.error(error)
  process.exit(1)
}
