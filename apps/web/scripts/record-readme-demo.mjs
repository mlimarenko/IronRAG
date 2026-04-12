#!/usr/bin/env node

import { chromium } from "playwright";
import { mkdtemp, mkdir, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { execFile } from "node:child_process";
import { promisify } from "node:util";

const execFileAsync = promisify(execFile);

const BASE_URL = process.env.IRONRAG_DEMO_BASE_URL ?? "http://127.0.0.1:19000";
const LOGIN = process.env.IRONRAG_DEMO_LOGIN ?? "admin";
const PASSWORD = process.env.IRONRAG_DEMO_PASSWORD ?? "rustrag123";
const OUTPUT_GIF =
  process.env.IRONRAG_DEMO_OUTPUT_GIF
  ?? path.resolve("/home/leader/sources/IronRAG/ironrag/docs/assets/readme-flow.gif");
const OUTPUT_WIDTH = Number.parseInt(process.env.IRONRAG_DEMO_OUTPUT_WIDTH ?? "1200", 10);
const VIEWPORT = {
  width: Number.parseInt(process.env.IRONRAG_DEMO_VIEWPORT_WIDTH ?? "1600", 10),
  height: Number.parseInt(process.env.IRONRAG_DEMO_VIEWPORT_HEIGHT ?? "1000", 10),
};
const VIDEO_FPS = Number.parseInt(process.env.IRONRAG_DEMO_FPS ?? "10", 10);
const STAGE_PAUSE = Number.parseInt(process.env.IRONRAG_DEMO_PAUSE_MS ?? "900", 10);
const QUERY_TEXT =
  process.env.IRONRAG_DEMO_QUERY
  ?? "Summarize organizations-100.csv in one sentence.";

const CURSOR_SCRIPT = `
(() => {
  const setup = () => {
    if (window.__ironragDemoCursorReady) return;
    window.__ironragDemoCursorReady = true;

    const style = document.createElement("style");
    style.textContent = \`
      #ironrag-demo-cursor {
        position: fixed;
        left: 0;
        top: 0;
        width: 20px;
        height: 20px;
        border-radius: 999px;
        background: rgba(17, 24, 39, 0.9);
        border: 2px solid rgba(255, 255, 255, 0.92);
        box-shadow: 0 6px 18px rgba(15, 23, 42, 0.22);
        pointer-events: none;
        z-index: 2147483646;
        transform: translate(-999px, -999px);
      }

      #ironrag-demo-cursor::after {
        content: "";
        position: absolute;
        inset: 5px;
        border-radius: 999px;
        background: rgba(255, 255, 255, 0.96);
      }

      #ironrag-demo-click {
        position: fixed;
        left: 0;
        top: 0;
        width: 18px;
        height: 18px;
        margin-left: -9px;
        margin-top: -9px;
        border-radius: 999px;
        border: 2px solid rgba(37, 99, 235, 0.72);
        pointer-events: none;
        opacity: 0;
        z-index: 2147483645;
      }

      #ironrag-demo-click.ironrag-demo-click-active {
        animation: ironragDemoPulse 360ms ease-out forwards;
      }

      @keyframes ironragDemoPulse {
        0% {
          opacity: 0.88;
          transform: scale(0.65);
        }
        100% {
          opacity: 0;
          transform: scale(2.4);
        }
      }
    \`;
    document.documentElement.append(style);

    const cursor = document.createElement("div");
    cursor.id = "ironrag-demo-cursor";
    const click = document.createElement("div");
    click.id = "ironrag-demo-click";
    document.body.append(cursor, click);

    window.__ironragDemoCursorMove = (x, y) => {
      cursor.style.transform = \`translate(\${x - 10}px, \${y - 10}px)\`;
      click.style.left = \`\${x}px\`;
      click.style.top = \`\${y}px\`;
    };

    window.__ironragDemoCursorClick = () => {
      click.classList.remove("ironrag-demo-click-active");
      void click.offsetWidth;
      click.classList.add("ironrag-demo-click-active");
    };
  };

  if (document.body) setup();
  else window.addEventListener("DOMContentLoaded", setup, { once: true });
})();
`;

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function moveCursor(page, from, to, duration = 380, steps = 18) {
  for (let index = 1; index <= steps; index += 1) {
    const progress = index / steps;
    const eased = 1 - (1 - progress) ** 3;
    const x = from.x + ((to.x - from.x) * eased);
    const y = from.y + ((to.y - from.y) * eased);
    await page.mouse.move(x, y);
    await page.evaluate(
      ([cursorX, cursorY]) => window.__ironragDemoCursorMove?.(cursorX, cursorY),
      [x, y],
    );
    await sleep(Math.max(8, Math.round(duration / steps)));
  }
  return { x: to.x, y: to.y };
}

async function clickLocator(page, locator, cursor, options = {}) {
  const { pauseAfter = STAGE_PAUSE } = options;
  await locator.scrollIntoViewIfNeeded();
  await locator.waitFor({ state: "visible" });
  const box = await locator.boundingBox();
  if (!box) {
    throw new Error("Could not resolve clickable bounds for locator");
  }
  const target = { x: box.x + (box.width / 2), y: box.y + (box.height / 2) };
  const nextCursor = await moveCursor(page, cursor, target, options.duration, options.steps);
  await page.evaluate(() => window.__ironragDemoCursorClick?.());
  await locator.click({ delay: 90 });
  await sleep(pauseAfter);
  return nextCursor;
}

async function typeInto(page, locator, value, cursor, options = {}) {
  const nextCursor = await clickLocator(page, locator, cursor, {
    pauseAfter: 220,
    duration: options.duration,
    steps: options.steps,
  });
  await locator.fill("");
  await locator.type(value, { delay: options.delay ?? 80 });
  await sleep(options.pauseAfter ?? 240);
  return nextCursor;
}

async function waitForGraphCanvas(page) {
  await page.waitForURL(/\/graph/);
  await page.getByPlaceholder("Search nodes...").waitFor({ state: "visible" });
  await page.locator("canvas").first().waitFor({ state: "visible", timeout: 20000 });
  await page.getByText(/\bREADY\b/i).waitFor({ state: "visible", timeout: 20000 });
}

async function run() {
  const tempRoot = await mkdtemp(path.join(tmpdir(), "ironrag-readme-demo-"));
  const videoDir = path.join(tempRoot, "video");
  const renderDir = path.join(tempRoot, "render");
  await mkdir(videoDir, { recursive: true });
  await mkdir(renderDir, { recursive: true });

  const browser = await chromium.launch({
    headless: true,
    executablePath: "/usr/bin/google-chrome",
    args: ["--no-sandbox", "--disable-dev-shm-usage"],
  });

  let context;
  try {
    context = await browser.newContext({
      viewport: VIEWPORT,
      colorScheme: "light",
      recordVideo: {
        dir: videoDir,
        size: VIEWPORT,
      },
    });

    await context.route("**/v1/query/sessions?**", async (route) => {
      if (route.request().method() !== "GET") {
        await route.continue();
        return;
      }
      await route.fulfill({
        status: 200,
        contentType: "application/json",
        body: "[]",
      });
    });

    await context.addInitScript(CURSOR_SCRIPT);
    const page = await context.newPage();
    let cursor = { x: 120, y: 120 };
    await page.goto(BASE_URL, { waitUntil: "networkidle" });
    await page.evaluate(([x, y]) => window.__ironragDemoCursorMove?.(x, y), [cursor.x, cursor.y]);
    await page.evaluate(() => {
      localStorage.setItem("ironrag_locale", "en");
    });

    await page.goto(`${BASE_URL}/login`, { waitUntil: "networkidle" });
    await page.waitForLoadState("networkidle");
    await sleep(700);

    cursor = await typeInto(page, page.getByLabel("Login").first(), LOGIN, cursor);
    cursor = await typeInto(page, page.getByLabel("Password").first(), PASSWORD, cursor, { delay: 90 });
    cursor = await clickLocator(page, page.getByRole("button", { name: "Sign In" }), cursor, { pauseAfter: 1200 });

    await page.waitForURL(/\/dashboard/, { timeout: 20000 });
    await page.waitForLoadState("networkidle");
    await sleep(700);

    const shellNav = page.getByRole("navigation");
    cursor = await clickLocator(page, shellNav.getByRole("button", { name: "Documents", exact: true }), cursor, { pauseAfter: 900 });
    await page.waitForURL(/\/documents/, { timeout: 20000 });
    await page.getByPlaceholder("Search documents...").waitFor({ state: "visible" });
    await sleep(500);
    cursor = await typeInto(
      page,
      page.getByPlaceholder("Search documents..."),
      "organizations",
      cursor,
      { delay: 70, pauseAfter: 500 },
    );
    const documentRow = page.getByText("organizations-100.csv").first();
    cursor = await clickLocator(page, documentRow, cursor, { pauseAfter: 1200 });
    await page.getByText("organizations-100.csv").nth(1).waitFor({ state: "visible", timeout: 10000 });
    await sleep(500);

    cursor = await clickLocator(page, shellNav.getByRole("button", { name: "Graph", exact: true }), cursor, { pauseAfter: 1000 });
    await waitForGraphCanvas(page);
    await sleep(1000);

    cursor = await clickLocator(page, page.getByRole("button", { name: "Sectors", exact: true }), cursor, { pauseAfter: 1200 });
    cursor = await clickLocator(page, page.getByRole("button", { name: "Components", exact: true }), cursor, { pauseAfter: 1200 });
    cursor = await clickLocator(page, page.getByRole("button", { name: "Bands", exact: true }), cursor, { pauseAfter: 1200 });
    cursor = await clickLocator(page, page.getByTitle("Organization").first(), cursor, { pauseAfter: 1200 });
    cursor = await clickLocator(page, page.getByRole("button", { name: /clear/i }), cursor, { pauseAfter: 900 });

    cursor = await clickLocator(page, shellNav.getByRole("button", { name: "AI Assistant", exact: true }), cursor, { pauseAfter: 1000 });
    await page.waitForURL(/\/assistant/, { timeout: 20000 });
    await page.getByRole("heading", { name: "AI Assistant", exact: true }).waitFor({ state: "visible" });
    await page.getByPlaceholder("Ask a question...").waitFor({ state: "visible" });
    await page.waitForLoadState("networkidle");
    await sleep(700);

    cursor = await typeInto(
      page,
      page.getByPlaceholder("Ask a question..."),
      QUERY_TEXT,
      cursor,
      { delay: 60, pauseAfter: 250 },
    );
    cursor = await clickLocator(page, page.locator("button").filter({ has: page.locator("svg.lucide-send") }).first(), cursor, { pauseAfter: 600 });
    await page.getByText("100-row company dataset", { exact: false }).waitFor({ state: "visible", timeout: 30000 });
    await sleep(2400);

    const pageVideo = page.video();
    await context.close();
    const recordedVideoPath = await pageVideo.path();
    const mp4Path = path.join(renderDir, "readme-demo.mp4");
    const palettePath = path.join(renderDir, "palette.png");

    await execFileAsync("ffmpeg", [
      "-y",
      "-i",
      recordedVideoPath,
      "-vf",
      `fps=${VIDEO_FPS},scale=${OUTPUT_WIDTH}:-1:flags=lanczos,palettegen=max_colors=160`,
      palettePath,
    ]);

    await execFileAsync("ffmpeg", [
      "-y",
      "-i",
      recordedVideoPath,
      "-movflags",
      "+faststart",
      "-pix_fmt",
      "yuv420p",
      "-vf",
      `scale=${OUTPUT_WIDTH}:-1:flags=lanczos`,
      mp4Path,
    ]);

    await execFileAsync("ffmpeg", [
      "-y",
      "-i",
      recordedVideoPath,
      "-i",
      palettePath,
      "-lavfi",
      `fps=${VIDEO_FPS},scale=${OUTPUT_WIDTH}:-1:flags=lanczos[x];[x][1:v]paletteuse=dither=bayer:bayer_scale=3:diff_mode=rectangle`,
      OUTPUT_GIF,
    ]);

    console.log(`Recorded demo video: ${recordedVideoPath}`);
    console.log(`Rendered preview mp4: ${mp4Path}`);
    console.log(`Rendered README gif: ${OUTPUT_GIF}`);
  } finally {
    if (context) {
      await context.close().catch(() => {});
    }
    await browser.close().catch(() => {});
    await rm(tempRoot, { recursive: true, force: true }).catch(() => {});
  }
}

run().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
