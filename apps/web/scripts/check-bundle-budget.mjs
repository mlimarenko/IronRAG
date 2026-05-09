#!/usr/bin/env node
/**
 * Frontend bundle budget gate.
 *
 * Runs after `vite build` and asserts that the **first-paint** chunks stay
 * under hand-set ceilings. Sprint 7 lazy-route work brought the main entry
 * down from ~810 KB gzip to ~85 KB gzip; this script stops a future commit
 * from re-eagerizing a heavy page (Sigma, Tiptap, Swagger UI) and blowing up
 * the initial paint without anyone noticing.
 *
 * Budgets cover gzipped size because that is what the browser actually
 * pulls down. Per-chunk budgets are intentionally generous (the goal is
 * "catch a 5x regression", not "block routine 5 KB drift").
 */
import { promises as fs } from 'node:fs';
import { gzipSync } from 'node:zlib';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const distDir = resolve(here, '../dist/assets');
const indexHtmlPath = resolve(here, '../dist/index.html');

// One entry per "first-paint" surface. The lazy route chunks (DocumentsPage,
// AdminPage, …) are not budgeted here because their cost is paid only when
// the user opens the corresponding tab.
const BUDGETS = [
  // Main entry: routing shell, AppContext, Toaster, login, dashboard, the
  // generated SDK + queries barrel. Sprint 5/6 npm install bumped several
  // runtime deps (react-router/dist 355 KB raw, tailwind-merge 100 KB raw,
  // sonner 64 KB raw, etc.) which grew the entry from ~82 KB gzip to
  // ~212 KB gzip with zero source change. The audited package-only floor is
  // 210.2 KB gzip; the ceiling keeps 15% headroom above that measured floor.
  { source: 'html-entry', gzipKbCeiling: 242, label: 'main entry' },
];

async function listBundleFiles() {
  const entries = await fs.readdir(distDir);
  return entries.filter((name) => name.endsWith('.js'));
}

async function listHtmlEntryFiles() {
  const html = await fs.readFile(indexHtmlPath, 'utf8');
  return [...html.matchAll(/<script\b[^>]*\bsrc="\/assets\/([^"]+\.js)"/g)].map(
    (match) => match[1],
  );
}

async function gzipSize(path) {
  const bytes = await fs.readFile(path);
  return gzipSync(bytes).length;
}

const failures = [];
const lines = [];

const files = await listBundleFiles();
const htmlEntryFiles = await listHtmlEntryFiles();
for (const budget of BUDGETS) {
  const candidates = budget.source === 'html-entry' ? htmlEntryFiles : files;
  const matches = candidates.filter((name) => files.includes(name));
  if (matches.length === 0) {
    failures.push(`Bundle budget: no files matched "${budget.source}" for "${budget.label}"`);
    continue;
  }
  for (const name of matches) {
    const size = await gzipSize(join(distDir, name));
    const kb = size / 1024;
    const ok = kb <= budget.gzipKbCeiling;
    lines.push(
      `${ok ? 'OK   ' : 'FAIL '}${name.padEnd(48)} ${kb.toFixed(1).padStart(7)} KB gzip / ${budget.gzipKbCeiling} KB ceiling (${budget.label})`,
    );
    if (!ok) {
      failures.push(
        `Bundle budget: ${name} is ${kb.toFixed(1)} KB gzip, ceiling is ${budget.gzipKbCeiling} KB. Run 'npm run build' and either trim imports or document the regression by raising the ceiling here.`,
      );
    }
  }
}

console.log(lines.join('\n'));

if (failures.length > 0) {
  console.error('\n' + failures.join('\n'));
  process.exit(1);
}
