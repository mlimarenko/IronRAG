#!/usr/bin/env node
// Large-graph interaction perf harness for the IronRAG web graph view.
//
// Drives the Sigma graph through the interactions a user actually performs
// (idle / wheel-zoom / pan / layout-mode switch / hover sweep / click-select)
// and records, per phase, the main-thread jank signals:
//   - long tasks (PerformanceObserver 'longtask', >50ms main-thread blocks)
//   - rAF frame intervals (p50 / p95 / max; >32ms = a dropped frame at 60fps)
//
// Everything is env-driven so it stays generic (no hardcoded host/corpus):
//   BASE_URL    frontend origin            (default http://127.0.0.1:19001)
//   API_URL     backend origin for login   (default http://127.0.0.1:19500)
//   LOGIN       user login                 (default admin)
//   PASSWORD    user password              (default changeme)
//   LIBRARY_ID  library to open in /graph  (optional; defaults to active)
//   HEADLESS    "0" to watch the run       (default headless)
//   ROUTE_API   "0" to disable /v1 reroute when BASE_URL != API_URL
//
// Run: node apps/web/scripts/graph-perf.mjs
import { mkdtemp } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import { chromium, firefox } from '@playwright/test'

import {
  buildBrowserLaunchOptions,
  getJankTag,
  parseSessionCookie,
  summarizeCpuProfile,
} from './graph-perf-lib.mjs'

const BASE_URL = process.env.BASE_URL || 'http://127.0.0.1:19001'
const API_URL = process.env.API_URL || 'http://127.0.0.1:19500'
const LOGIN = process.env.LOGIN || 'admin'
const PASSWORD = process.env.PASSWORD || 'changeme'
const LIBRARY_ID = process.env.LIBRARY_ID || ''
const WORKSPACE_ID = process.env.WORKSPACE_ID || ''
const HEADLESS = process.env.HEADLESS !== '0'
const ROUTE_API = process.env.ROUTE_API !== '0'
const EDGE_DENSITY_TOGGLE_SELECTOR = '[data-perf-id="edge-density-toggle"]'
const DEFAULT_GPU_ARGS = '--use-gl=egl --enable-gpu --ignore-gpu-blocklist --no-sandbox'

const sleep = (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds))

// Injected before any page script: a global frame/longtask profiler keyed by
// a "phase" label the harness flips on/off around each interaction.
const PROFILER = `
(() => {
  const P = (window.__perf = { phase: null, frames: [], longtasks: [], last: 0 });
  const raf = window.requestAnimationFrame.bind(window);
  function tick(t) {
    if (P.phase) {
      if (P.last) P.frames.push(t - P.last);
      P.last = t;
    } else {
      P.last = 0;
    }
    raf(tick);
  }
  raf(tick);
  P.all = [];
  try {
    new PerformanceObserver((list) => {
      for (const entry of list.getEntries()) {
        P.all.push(entry.duration);
        if (P.phase) P.longtasks.push(entry.duration);
      }
    }).observe({ entryTypes: ['longtask'] });
  } catch (error) {
    console.debug('longtask observations are unavailable', error);
  }
  window.__perfStart = () => { P.phase = true; P.frames = []; P.longtasks = []; P.last = 0; };
  window.__perfStop = () => {
    P.phase = null;
    const frames = P.frames.slice().sort((left, right) => left - right);
    const quantile = (percentile) => frames.length
      ? frames[Math.min(frames.length - 1, Math.floor(percentile * frames.length))]
      : 0;
    const longTasks = P.longtasks;
    return {
      frames: frames.length,
      fps: frames.length ? Math.round(1000 / (frames.reduce((left, right) => left + right, 0) / frames.length)) : 0,
      p50: +quantile(0.5).toFixed(1),
      p95: +quantile(0.95).toFixed(1),
      max: +(frames.at(-1) || 0).toFixed(1),
      dropped: frames.filter((interval) => interval > 32).length,
      longtasks: longTasks.length,
      longtaskMs: +longTasks.reduce((left, right) => left + right, 0).toFixed(0),
      longtaskMax: +Math.max(0, ...longTasks).toFixed(0),
    };
  };
})();
`

async function measure(page, label, interaction, settleMilliseconds = 400) {
  await page.evaluate(() => window.__perfStart())
  await interaction()
  await sleep(settleMilliseconds)
  const result = await page.evaluate(() => window.__perfStop())
  const tag = getJankTag(result.longtaskMax)

  console.log(
    `${label.padEnd(20)} fps=${String(result.fps).padStart(3)}  p50=${String(result.p50).padStart(5)}ms  p95=${String(
      result.p95,
    ).padStart(
      6,
    )}ms  max=${String(result.max).padStart(6)}ms  dropped=${String(result.dropped).padStart(3)}  ` +
      `longtasks=${String(result.longtasks).padStart(2)} total=${String(result.longtaskMs).padStart(5)}ms worst=${String(
        result.longtaskMax,
      ).padStart(5)}ms${tag}`,
  )

  return result
}

async function logIn() {
  console.log(`login ${LOGIN} @ ${API_URL} ...`)
  const response = await fetch(`${API_URL}/v1/iam/session/login`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ login: LOGIN, password: PASSWORD }),
  })

  if (!response.ok) {
    throw new Error(`login failed: ${response.status}`)
  }

  const cookie = parseSessionCookie(response.headers.get('set-cookie') || '')
  if (!cookie) {
    throw new Error('no session cookie returned')
  }

  console.log(`session cookie: ${cookie[0]}`)
  return cookie
}

async function launchBrowser() {
  const browserName = (process.env.BROWSER || 'chromium').toLowerCase()
  const engine = browserName === 'firefox' ? firefox : chromium
  const options = buildBrowserLaunchOptions(
    browserName,
    HEADLESS,
    process.env.GPU_ARGS || DEFAULT_GPU_ARGS,
  )
  const browser = await engine.launch(options)

  console.log(`browser: ${browserName === 'firefox' ? 'firefox' : 'chromium'}`)
  return { browser, browserName }
}

async function createContext(browser, cookie) {
  const context = await browser.newContext({ viewport: { width: 1680, height: 1000 } })
  const url = new URL(BASE_URL)

  await context.addCookies([{ name: cookie[0], value: cookie[1], domain: url.hostname, path: '/' }])
  await context.addInitScript(PROFILER)

  if (LIBRARY_ID || WORKSPACE_ID) {
    await context.addInitScript(
      ({ libraryId, workspaceId }) => {
        if (libraryId) localStorage.setItem('ironrag_active_library', libraryId)
        if (workspaceId) localStorage.setItem('ironrag_active_workspace', workspaceId)
      },
      { libraryId: LIBRARY_ID, workspaceId: WORKSPACE_ID },
    )
  }

  return context
}

async function configureApiRouting(page) {
  if (!ROUTE_API || API_URL === BASE_URL) {
    return
  }

  const apiOrigin = new URL(API_URL).origin
  await page.route('**/v1/**', (route) => {
    const requestUrl = new URL(route.request().url())
    return route.continue({ url: `${apiOrigin}${requestUrl.pathname}${requestUrl.search}` })
  })
  console.log(`route /v1/* -> ${apiOrigin}`)
}

async function waitForGraph(page, noCanvasScreenshot) {
  const graphUrl = `${BASE_URL}/graph`
  console.log(`open ${graphUrl} (library via localStorage=${LIBRARY_ID || 'active'})`)
  await page.goto(graphUrl, { waitUntil: 'domcontentloaded' })
  await page.waitForLoadState('networkidle').catch(() => undefined)

  const canvas = await page.waitForSelector('canvas', { timeout: 60000 }).catch(async () => {
    const pageText = (await page.evaluate(() => document.body.innerText))
      .slice(0, 300)
      .replaceAll('\n', ' | ')
    console.log('  no canvas — page text:', pageText)
    await page.screenshot({ path: noCanvasScreenshot })
    throw new Error(`canvas not found (screenshot: ${noCanvasScreenshot})`)
  })

  await waitForGraphLayout(page)
  const box = await canvas.boundingBox()
  if (!box) {
    throw new Error('canvas has no bounding box')
  }

  return box
}

async function waitForGraphLayout(page) {
  await page
    .waitForFunction(
      () => {
        let digitCount = 0
        for (const character of document.body.innerText) {
          digitCount = character >= '0' && character <= '9' ? digitCount + 1 : 0
          if (digitCount === 3) return true
        }
        return false
      },
      { timeout: 90000 },
    )
    .catch(() => undefined)
  await page
    .waitForFunction(
      () => {
        const canvas = document.querySelector('canvas')
        return canvas && canvas.getBoundingClientRect().width > 1000
      },
      { timeout: 60000 },
    )
    .catch(() => console.log('  WARNING: canvas never reached full size'))
  await sleep(3000)
}

async function enableRequestedEdgeDensity(page) {
  if (process.env.SHOW_ALL !== '1') {
    return
  }

  await page
    .waitForSelector(EDGE_DENSITY_TOGGLE_SELECTOR, { timeout: 15000 })
    .catch(() => undefined)
  const clicked = await page.evaluate((selector) => {
    const button = document.querySelector(selector)
    if (!button) return false
    button.click()
    return true
  }, EDGE_DENSITY_TOGGLE_SELECTOR)
  console.log(`edge-density toggle: ${clicked ? 'clicked' : 'NOT FOUND'}`)
  await sleep(5000)
}

async function logInitialLoadStats(page) {
  const stats = await page.evaluate(() => {
    const longTasks = window.__perf?.all ?? []
    const total = longTasks.reduce((sum, duration) => sum + duration, 0)
    const worst = longTasks.reduce((maximum, duration) => Math.max(maximum, duration), 0)
    return { count: longTasks.length, total: Math.round(total), worst: Math.round(worst) }
  })
  const freezeTag = stats.worst > 200 ? ' <== load freeze' : ''
  console.log(
    `initial-load        longtasks=${stats.count} total=${stats.total}ms worst=${stats.worst}ms${freezeTag}`,
  )
}

async function startCpuProfiler(context, page, browserName, interval) {
  if (browserName !== 'chromium') {
    return null
  }

  const profiler = await context.newCDPSession(page)
  await profiler.send('Profiler.enable')
  await profiler.send('Profiler.setSamplingInterval', { interval })
  await profiler.send('Profiler.start')
  return profiler
}

async function logCpuProfile(profiler, heading, padding) {
  if (!profiler) {
    return
  }

  const { profile } = await profiler.send('Profiler.stop')
  const entries = summarizeCpuProfile(profile)
  console.log(heading)
  for (const [key, microseconds] of entries) {
    console.log(`  ${String(Math.round(microseconds / 1000)).padStart(padding)}ms  ${key}`)
  }
  console.log('')
}

async function measurePan(page, context, browserName, centerX, centerY) {
  const profiler = await startCpuProfiler(context, page, browserName, 200)
  await measure(page, 'pan', async () => {
    await page.mouse.move(centerX, centerY)
    await page.mouse.down()
    for (let index = 0; index < 20; index += 1) {
      await page.mouse.move(centerX - index * 12, centerY - index * 6)
      await sleep(16)
    }
    await page.mouse.up()
  })
  await logCpuProfile(profiler, '\n  --- pan CPU self-time (top functions, ms) ---', 6)
}

async function measureInteractions(page, context, browserName, centerX, centerY) {
  console.log('\n=== per-interaction frame / long-task profile ===')
  await measure(page, 'idle', () => sleep(1500))
  await measure(page, 'wheel-zoom', async () => {
    for (let index = 0; index < 8; index += 1) {
      await page.mouse.move(centerX, centerY)
      await page.mouse.wheel(0, index % 2 ? 240 : -240)
      await sleep(120)
    }
  })
  await measurePan(page, context, browserName, centerX, centerY)
  await measure(page, 'hover-sweep', async () => {
    for (let index = 0; index < 30; index += 1) {
      await page.mouse.move(centerX - 120 + index * 8, centerY + Math.sin(index / 3) * 40)
      await sleep(40)
    }
    await sleep(400)
  })
  await measure(page, 'click-select', async () => {
    await page.mouse.move(centerX, centerY)
    await page.mouse.click(centerX, centerY)
    await sleep(600)
  })
  await measure(page, 'node-drag', async () => {
    await page.mouse.move(centerX, centerY)
    await page.mouse.down()
    for (let index = 0; index < 20; index += 1) {
      await page.mouse.move(centerX + index * 8, centerY + index * 4)
      await sleep(16)
    }
    await page.mouse.up()
  })
}

async function listLayoutButtons(page) {
  const buttons = []
  for (const button of await page.$$('button[aria-pressed]')) {
    if ((await button.getAttribute('data-perf-id')) === 'edge-density-toggle') {
      continue
    }
    buttons.push({ button, label: (await button.getAttribute('aria-label')) || '' })
  }
  return buttons
}

async function measureLayoutSwitches(page, context, browserName) {
  const buttons = await listLayoutButtons(page)
  console.log(`
(layout buttons: ${buttons.map(({ label }) => label).join(', ')})`)
  let hasProfiledSwitch = false

  for (const { button, label } of buttons) {
    if ((await button.getAttribute('aria-pressed')) === 'true') {
      continue
    }

    const profiler = hasProfiledSwitch
      ? null
      : await startCpuProfiler(context, page, browserName, 150)
    await measure(
      page,
      `mode→${label.slice(0, 14)}`,
      async () => {
        await button.click().catch(() => undefined)
        await sleep(2500)
      },
      800,
    )
    await logCpuProfile(profiler, '  --- mode-switch CPU self-time (top, ms) ---', 5)
    hasProfiledSwitch ||= profiler !== null
  }
}

async function runGraphPerf() {
  const artifactDirectory = await mkdtemp(join(tmpdir(), 'ironrag-graph-perf-'))
  const noCanvasScreenshot = join(artifactDirectory, 'no-canvas.png')
  const cookie = await logIn()
  const { browser, browserName } = await launchBrowser()

  try {
    const context = await createContext(browser, cookie)
    const page = await context.newPage()
    page.on('pageerror', (error) => console.log('  [pageerror]', error.message))
    await configureApiRouting(page)

    const box = await waitForGraph(page, noCanvasScreenshot)
    await enableRequestedEdgeDensity(page)
    console.log(
      `canvas ${Math.round(box.width)}x${Math.round(box.height)} at (${Math.round(box.x)},${Math.round(box.y)})`,
    )
    await logInitialLoadStats(page)

    const centerX = box.x + box.width * 0.62
    const centerY = box.y + box.height * 0.45
    await measureInteractions(page, context, browserName, centerX, centerY)
    await measureLayoutSwitches(page, context, browserName)
    console.log('\ndone.')
  } finally {
    await browser.close()
  }
}

try {
  await runGraphPerf()
} catch (error) {
  console.error(error)
  process.exitCode = 1
}
