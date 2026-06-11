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
// Run:  node /tmp/graph-perf.mjs
import { chromium, firefox } from 'playwright';

const BASE_URL = process.env.BASE_URL || 'http://127.0.0.1:19001';
const API_URL = process.env.API_URL || 'http://127.0.0.1:19500';
const LOGIN = process.env.LOGIN || 'admin';
const PASSWORD = process.env.PASSWORD || 'changeme';
const LIBRARY_ID = process.env.LIBRARY_ID || '';
const WORKSPACE_ID = process.env.WORKSPACE_ID || '';
const HEADLESS = process.env.HEADLESS !== '0';
const ROUTE_API = process.env.ROUTE_API !== '0';
const EDGE_DENSITY_TOGGLE_SELECTOR = '[data-perf-id="edge-density-toggle"]';

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

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
      for (const e of list.getEntries()) {
        P.all.push(e.duration); // every long task since load (for initial-load report)
        if (P.phase) P.longtasks.push(e.duration);
      }
    }).observe({ entryTypes: ['longtask'] });
  } catch (e) {}
  window.__perfStart = () => { P.phase = true; P.frames = []; P.longtasks = []; P.last = 0; };
  window.__perfStop = () => {
    P.phase = null;
    const f = P.frames.slice().sort((a, b) => a - b);
    const q = (p) => (f.length ? f[Math.min(f.length - 1, Math.floor(p * f.length))] : 0);
    const lt = P.longtasks;
    return {
      frames: f.length,
      fps: f.length ? Math.round(1000 / (f.reduce((a, b) => a + b, 0) / f.length)) : 0,
      p50: +q(0.5).toFixed(1),
      p95: +q(0.95).toFixed(1),
      max: +(f[f.length - 1] || 0).toFixed(1),
      dropped: f.filter((x) => x > 32).length,
      longtasks: lt.length,
      longtaskMs: +lt.reduce((a, b) => a + b, 0).toFixed(0),
      longtaskMax: +Math.max(0, ...lt).toFixed(0),
    };
  };
})();
`;

async function measure(page, label, fn, settleMs = 400) {
  await page.evaluate(() => window.__perfStart());
  await fn();
  await sleep(settleMs);
  const r = await page.evaluate(() => window.__perfStop());
  const tag =
    r.longtaskMax > 200 ? ' <== SEVERE' : r.longtaskMax > 80 ? ' <- janky' : '';
  console.log(
    `${label.padEnd(20)} fps=${String(r.fps).padStart(3)}  p50=${String(r.p50).padStart(5)}ms  p95=${String(
      r.p95,
    ).padStart(6)}ms  max=${String(r.max).padStart(6)}ms  dropped=${String(r.dropped).padStart(3)}  ` +
      `longtasks=${String(r.longtasks).padStart(2)} total=${String(r.longtaskMs).padStart(5)}ms worst=${String(
        r.longtaskMax,
      ).padStart(5)}ms${tag}`,
  );
  return r;
}

(async () => {
  // 1) login via API to get the session cookie, hand it to the browser context
  console.log(`login ${LOGIN} @ ${API_URL} ...`);
  const loginRes = await fetch(`${API_URL}/v1/iam/session/login`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ login: LOGIN, password: PASSWORD }),
  });
  if (!loginRes.ok) throw new Error(`login failed: ${loginRes.status}`);
  const setCookie = loginRes.headers.get('set-cookie') || '';
  const m = setCookie.match(/([^=;,\s]+)=([^;]+)/);
  if (!m) throw new Error('no session cookie returned');
  const [, cookieName, cookieValue] = m;
  console.log(`session cookie: ${cookieName}`);

  const engine = (process.env.BROWSER || 'chromium').toLowerCase() === 'firefox' ? firefox : chromium;
  const browser =
    engine === firefox
      ? await firefox.launch({ headless: HEADLESS })
      : await chromium.launch({
          headless: HEADLESS,
          args: (
            process.env.GPU_ARGS || '--use-gl=egl --enable-gpu --ignore-gpu-blocklist --no-sandbox'
          ).split(' '),
        });
  console.log(`browser: ${engine === firefox ? 'firefox' : 'chromium'}`);
  const ctx = await browser.newContext({ viewport: { width: 1680, height: 1000 } });
  const url = new URL(BASE_URL);
  await ctx.addCookies([
    { name: cookieName, value: cookieValue, domain: url.hostname, path: '/' },
  ]);
  await ctx.addInitScript(PROFILER);
  // The graph view reads its active library/workspace from localStorage
  // (ironrag_active_library / ironrag_active_workspace), not a URL param.
  if (LIBRARY_ID || WORKSPACE_ID) {
    await ctx.addInitScript(
      ({ lib, ws }) => {
        if (lib) localStorage.setItem('ironrag_active_library', lib);
        if (ws) localStorage.setItem('ironrag_active_workspace', ws);
      },
      { lib: LIBRARY_ID, ws: WORKSPACE_ID },
    );
  }
  const page = await ctx.newPage();
  if (ROUTE_API && API_URL !== BASE_URL) {
    const apiOrigin = new URL(API_URL).origin;
    await page.route('**/v1/**', (route) => {
      const nextUrl = new URL(route.request().url());
      const routedUrl = `${apiOrigin}${nextUrl.pathname}${nextUrl.search}`;
      void route.continue({ url: routedUrl });
    });
    console.log(`route /v1/* -> ${apiOrigin}`);
  }
  page.on('pageerror', (e) => console.log('  [pageerror]', e.message));

  const graphUrl = `${BASE_URL}/graph`;
  console.log(`open ${graphUrl} (library via localStorage=${LIBRARY_ID || 'active'})`);
  await page.goto(graphUrl, { waitUntil: 'domcontentloaded' });
  await page.waitForLoadState('networkidle').catch(() => {});

  // wait for the canvas + node-count text to confirm the graph is rendered
  const canvas = await page.waitForSelector('canvas', { timeout: 60000 }).catch(async () => {
    console.log('  no canvas — page text:', (await page.evaluate(() => document.body.innerText)).slice(0, 300).replace(/\\n/g, ' | '));
    await page.screenshot({ path: '/tmp/graph-perf-nocanvas.png' });
    throw new Error('canvas not found (see /tmp/graph-perf-nocanvas.png)');
  });
  void canvas;
  // wait until the graph payload has been loaded + laid out (node count shows)
  await page
    .waitForFunction(() => /\d{3,}/.test(document.body.innerText), { timeout: 90000 })
    .catch(() => {});
  // CRITICAL: wait for the canvas to reach its real on-screen size — Sigma
  // sizes its canvas to the container, which may still be 300x150 (the
  // <canvas> intrinsic default) until layout settles. Measuring at 300x150
  // renders into a trivially small viewport and reports false 60fps.
  await page
    .waitForFunction(
      () => {
        const c = document.querySelector('canvas');
        return c && c.getBoundingClientRect().width > 1000;
      },
      { timeout: 60000 },
    )
    .catch(() => console.log('  WARNING: canvas never reached full size'));
  await sleep(3000); // let initial layout + first render settle
  // Optionally enable the edge-density toggle before measuring, to match a
  // user who asked for more visual edge context. `SHOW_ALL` is retained as the
  // env var name for compatibility with existing perf notes.
  if (process.env.SHOW_ALL === '1') {
    await page.waitForSelector(EDGE_DENSITY_TOGGLE_SELECTOR, { timeout: 15000 }).catch(() => {});
    const toggled = await page.evaluate((selector) => {
      const btn = document.querySelector(selector);
      if (btn) { btn.click(); return true; }
      return false;
    }, EDGE_DENSITY_TOGGLE_SELECTOR);
    console.log(`edge-density toggle: ${toggled ? 'clicked' : 'NOT FOUND'}`);
    await sleep(5000); // graph rebuild with denser edge sample + settle
  }
  const box = await (await page.$('canvas')).boundingBox();
  console.log(`canvas ${Math.round(box.width)}x${Math.round(box.height)} at (${Math.round(box.x)},${Math.round(box.y)})`);
  // Initial-load cost: longest main-thread block from navigation until the
  // graph is ready (NDJSON parse + large Graphology build + first layout).
  // This is a one-time freeze the user feels when opening
  // /graph; the per-interaction phases below start AFTER it settles.
  const loadStats = await page.evaluate(() => {
    const lt = (window.__perf && window.__perf.all) || [];
    const total = lt.reduce((a, d) => a + d, 0);
    const worst = lt.reduce((m, d) => Math.max(m, d), 0);
    return { count: lt.length, total: Math.round(total), worst: Math.round(worst) };
  });
  console.log(
    `initial-load        longtasks=${loadStats.count} total=${loadStats.total}ms worst=${loadStats.worst}ms` +
      `${loadStats.worst > 200 ? ' <== load freeze' : ''}`,
  );

  const cx = box.x + box.width * 0.62; // dense node band is right-of-center
  const cy = box.y + box.height * 0.45;

  console.log('\n=== per-interaction frame / long-task profile ===');

  await measure(page, 'idle', async () => {
    await sleep(1500);
  });

  await measure(page, 'wheel-zoom', async () => {
    for (let i = 0; i < 8; i++) {
      await page.mouse.move(cx, cy);
      await page.mouse.wheel(0, i % 2 ? 240 : -240);
      await sleep(120);
    }
  });

  // CPU-profile the pan phase to find the hot function (Chromium/CDP only)
  const cdp = engine === chromium ? await ctx.newCDPSession(page) : null;
  if (cdp) {
    await cdp.send('Profiler.enable');
    await cdp.send('Profiler.setSamplingInterval', { interval: 200 });
    await cdp.send('Profiler.start');
  }
  await measure(page, 'pan', async () => {
    await page.mouse.move(cx, cy);
    await page.mouse.down();
    for (let i = 0; i < 20; i++) {
      await page.mouse.move(cx - i * 12, cy - i * 6);
      await sleep(16);
    }
    await page.mouse.up();
  });
  const { profile } = cdp ? await cdp.send('Profiler.stop') : { profile: null };
  if (profile) {
    // aggregate self time by function
    const byFn = new Map();
    const nodeById = new Map(profile.nodes.map((n) => [n.id, n]));
    const dt = profile.timeDeltas || [];
    const samples = profile.samples || [];
    for (let i = 0; i < samples.length; i++) {
      const n = nodeById.get(samples[i]);
      if (!n) continue;
      const cf = n.callFrame;
      const key = `${cf.functionName || '(anon)'} @ ${(cf.url || '').split('/').pop()}:${cf.lineNumber}`;
      byFn.set(key, (byFn.get(key) || 0) + (dt[i] || 0));
    }
    const top = [...byFn.entries()].sort((a, b) => b[1] - a[1]).slice(0, 14);
    console.log('\n  --- pan CPU self-time (top functions, µs) ---');
    for (const [k, us] of top) console.log(`  ${String(Math.round(us / 1000)).padStart(6)}ms  ${k}`);
    console.log('');
  }

  await measure(page, 'hover-sweep', async () => {
    for (let i = 0; i < 30; i++) {
      await page.mouse.move(cx - 120 + i * 8, cy + Math.sin(i / 3) * 40);
      await sleep(40);
    }
    await sleep(400); // dwell so the hover branch actually commits
  });

  await measure(page, 'click-select', async () => {
    await page.mouse.move(cx, cy);
    await page.mouse.click(cx, cy);
    await sleep(600);
  });

  await measure(page, 'node-drag', async () => {
    await page.mouse.move(cx, cy);
    await page.mouse.down();
    for (let i = 0; i < 20; i++) {
      await page.mouse.move(cx + i * 8, cy + i * 4);
      await sleep(16);
    }
    await page.mouse.up();
  });

  // REAL layout-mode switches. The layout picker buttons carry aria-pressed;
  // we click the INACTIVE ones (clicking the active one is a no-op and gives a
  // false 120fps). Each click changes `layout`, which recomputes positions
  // (off-thread worker) and re-applies them — the "изменение представления"
  // path that used to lag on dense graphs. Skip the edge-density toggle.
  const ariaBtns = await page.$$('button[aria-pressed]');
  const layoutBtns = [];
  for (const btn of ariaBtns) {
    const label = (await btn.getAttribute('aria-label')) || '';
    if ((await btn.getAttribute('data-perf-id')) === 'edge-density-toggle') continue;
    layoutBtns.push({ btn, label });
  }
  console.log(`\n(layout buttons: ${layoutBtns.map((b) => b.label).join(', ')})`);
  let profiledSwitch = false;
  for (const { btn, label } of layoutBtns) {
    const pressed = await btn.getAttribute('aria-pressed');
    if (pressed === 'true') continue; // already active → would be a no-op
    // CPU-profile the FIRST real switch to find the hot function.
    const cdp2 = !profiledSwitch && engine === chromium ? await ctx.newCDPSession(page) : null;
    if (cdp2) {
      await cdp2.send('Profiler.enable');
      await cdp2.send('Profiler.setSamplingInterval', { interval: 150 });
      await cdp2.send('Profiler.start');
    }
    await measure(
      page,
      `mode→${label.slice(0, 14)}`,
      async () => {
        await btn.click().catch(() => {});
        await sleep(2500); // worker layout recompute + apply + settle
      },
      800,
    );
    if (cdp2) {
      const { profile } = await cdp2.send('Profiler.stop');
      const byFn = new Map();
      const nodeById = new Map(profile.nodes.map((n) => [n.id, n]));
      const dt = profile.timeDeltas || [];
      const samples = profile.samples || [];
      for (let i = 0; i < samples.length; i++) {
        const n = nodeById.get(samples[i]);
        if (!n) continue;
        const cf = n.callFrame;
        const key = `${cf.functionName || '(anon)'} @ ${(cf.url || '').split('/').pop()}:${cf.lineNumber}`;
        byFn.set(key, (byFn.get(key) || 0) + (dt[i] || 0));
      }
      const top = [...byFn.entries()].sort((a, b) => b[1] - a[1]).slice(0, 14);
      console.log('  --- mode-switch CPU self-time (top, ms) ---');
      for (const [k, us] of top) console.log(`  ${String(Math.round(us / 1000)).padStart(5)}ms  ${k}`);
      profiledSwitch = true;
    }
  }

  console.log('\ndone.');
  await browser.close();
})().catch((e) => {
  console.error(e);
  process.exit(1);
});
