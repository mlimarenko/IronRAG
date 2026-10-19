import { test, expect } from '@playwright/test'

/**
 * Perf sanity on a large reference library. The identifiers and minimum
 * expected topology size are read from environment variables so local and CI
 * runs can each point at their own fixture without repository-bound operator
 * data.
 *
 * Required env vars:
 *   IRONRAG_PERF_LIBRARY_ID   uuid of the large library to probe
 *   IRONRAG_PERF_WORKSPACE_ID uuid of the workspace that owns it
 *   IRONRAG_PERF_MIN_NODE_COUNT optional minimum expected nodes
 *   IRONRAG_PERF_MIN_EDGE_COUNT optional minimum expected edges
 *
 * We measure:
 *   1. Cold graph topology fetch (backend)
 *   2. Time until <canvas> paints in /graph (UI cold render)
 *   3. Filter-by-search latency (the reducer-pipeline fix — must be
 *      visible within one Sigma refresh, not seconds)
 *
 * Results go to stdout; Playwright reporter captures them.
 */

const PERF_LIBRARY_ID = process.env.IRONRAG_PERF_LIBRARY_ID
const PERF_WORKSPACE_ID = process.env.IRONRAG_PERF_WORKSPACE_ID
const PERF_MIN_NODE_COUNT = Number.parseInt(process.env.IRONRAG_PERF_MIN_NODE_COUNT ?? '1000', 10)
const PERF_MIN_EDGE_COUNT = Number.parseInt(process.env.IRONRAG_PERF_MIN_EDGE_COUNT ?? '1000', 10)
const PERF_FILTER_BUDGET_MS = Number.parseInt(
  process.env.IRONRAG_PERF_FILTER_BUDGET_MS ?? '1500',
  10,
)

const hasPerfFixture = Boolean(PERF_LIBRARY_ID && PERF_WORKSPACE_ID)

test.describe('large library perf', () => {
  test.skip(!hasPerfFixture, 'IRONRAG_PERF_LIBRARY_ID and IRONRAG_PERF_WORKSPACE_ID are required')

  test.beforeEach(async ({ page }) => {
    await page.addInitScript(
      ({ libId, wsId }) => {
        localStorage.setItem('ironrag.v1.active-library', libId)
        localStorage.setItem('ironrag.v1.active-workspace', wsId)
      },
      { libId: PERF_LIBRARY_ID ?? '', wsId: PERF_WORKSPACE_ID ?? '' },
    )
  })

  test('graph page cold render + search filter latency', async ({ page }) => {
    page.on('console', (msg) => console.log(`[browser ${msg.type()}] ${msg.text()}`))
    page.on('pageerror', (err) => console.log(`[browser error] ${err.message}`))

    const navStart = Date.now()
    await page.goto('/graph', { waitUntil: 'domcontentloaded' })
    await page.waitForLoadState('networkidle', { timeout: 30_000 }).catch(() => {})
    console.log(`[perf] URL after goto: ${page.url()}`)

    // Wait for Sigma to paint its canvas — the first visible <canvas>
    // under the graph area is our "first render" marker.
    const canvas = page.locator('canvas').first()
    await canvas.waitFor({ state: 'visible', timeout: 60_000 })
    const tFirstPaint = Date.now() - navStart
    console.log(`[perf] TTI first canvas paint: ${tFirstPaint} ms`)

    // Read the topology metadata from the toolbar to confirm we landed
    // on the real target library, not a fallback.
    const nodeCountText = await page
      .getByText(/\d[\d\s,.]*\s*(nodes|узлов)/i)
      .first()
      .textContent()
    const edgeCountText = await page
      .getByText(/\d[\d\s,.]*\s*(edges|рёбер)/i)
      .first()
      .textContent()
    const parseCount = (value: string | null) =>
      Number.parseInt(value?.match(/[\d\s,.]+/u)?.[0].replaceAll(/[^\d]/gu, '') ?? '', 10)
    const nodeCount = parseCount(nodeCountText)
    const edgeCount = parseCount(edgeCountText)
    expect(nodeCount).toBeGreaterThanOrEqual(PERF_MIN_NODE_COUNT)
    expect(edgeCount).toBeGreaterThanOrEqual(PERF_MIN_EDGE_COUNT)
    console.log(`[perf] toolbar: ${nodeCountText?.trim()} / ${edgeCountText?.trim()}`)

    await page.screenshot({
      path: 'visual-qa/screenshots/desktop-graph-large-cold.png',
      fullPage: true,
    })

    // Filter latency. The reducer-pipeline fix means typing in the
    // search box must NOT trigger a Graphology rebuild. We measure from
    // the keystroke to when the toolbar settles. The 250 ms debounce
    // inside GraphPage is part of the budget.
    const searchInput = page.getByPlaceholder(/search|поиск/i).first()
    await searchInput.waitFor({ state: 'visible', timeout: 5_000 })

    const filterStart = Date.now()
    await searchInput.fill(`__ironrag_perf_no_match_${Date.now()}__`)
    await expect(page.locator('.graph-no-match')).toBeVisible({ timeout: PERF_FILTER_BUDGET_MS })
    const tFilter = Date.now() - filterStart
    console.log(`[perf] search filter apply: ${tFilter} ms (includes 250ms debounce)`)
    expect(tFilter).toBeLessThanOrEqual(PERF_FILTER_BUDGET_MS)

    await page.screenshot({
      path: 'visual-qa/screenshots/desktop-graph-large-filtered.png',
      fullPage: true,
    })

    // Clear filter to return to full set.
    await searchInput.fill('')
    await expect(page.locator('.graph-no-match')).toHaveCount(0, { timeout: PERF_FILTER_BUDGET_MS })
    await page.screenshot({
      path: 'visual-qa/screenshots/desktop-graph-large-cleared.png',
      fullPage: true,
    })

    expect(tFirstPaint).toBeLessThan(60_000)
  })

  test('backend graph topology cold + warm', async ({ request }) => {
    const url = `/v1/knowledge/libraries/${PERF_LIBRARY_ID}/graph`
    const timings: number[] = []
    for (let i = 0; i < 5; i += 1) {
      const start = Date.now()
      const res = await request.get(url)
      expect(res.status()).toBe(200)
      const bytes = (await res.body()).byteLength
      const elapsed = Date.now() - start
      timings.push(elapsed)
      console.log(`[perf] backend topology run ${i + 1}: ${elapsed} ms, ${bytes} bytes`)
    }
    const median = [...timings].sort((a, b) => a - b)[Math.floor(timings.length / 2)]
    console.log(`[perf] backend topology median: ${median} ms`)
  })
})
