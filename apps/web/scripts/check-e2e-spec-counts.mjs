#!/usr/bin/env node
/**
 * End-to-end spec gate.
 *
 * A Playwright run that collects nothing still exits 0 and prints nothing that
 * looks like a failure — the same trap `BACKEND_INTEGRATION_SUITES` closes for
 * `cargo test`. A spec file that stops being collected (renamed, moved out of
 * `testDir`, excluded by a `testMatch` change) is then indistinguishable from
 * a green run, which is how `tests/e2e` came to hold specs no pipeline had
 * ever executed (IRG-85).
 *
 * So the manifest, not the runner, decides what a complete run is:
 * `tests/e2e/spec-manifest.json` declares how many tests each spec file
 * contributes, and this gate compares it with the run's own JSON report. A
 * file that ran fewer tests, a file that disappeared, a file the run reports
 * but the manifest does not declare, and an empty run each fail. A new spec is
 * declared in the same commit that adds it.
 *
 * Usage: node scripts/check-e2e-spec-counts.mjs <playwright-json-report>
 */
import { promises as fs } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))
export const manifestPath = resolve(here, '../tests/e2e/spec-manifest.json')
export const specDir = resolve(here, '../tests/e2e')

/** Count the tests each file contributes, following nested describe blocks. */
export function observedCounts(report) {
  const counts = new Map()
  const walk = (suite, fileName) => {
    const name = suite.file ?? fileName
    for (const spec of suite.specs ?? []) {
      const runs = spec.tests?.length || 1
      counts.set(name, (counts.get(name) ?? 0) + runs)
    }
    for (const nested of suite.suites ?? []) walk(nested, name)
  }
  for (const suite of report.suites ?? []) walk(suite, suite.file ?? '<unknown>')
  return counts
}

/** Order findings by spec file name, so the same mismatch reads the same way every run. */
const byName = (a, b) => a.localeCompare(b)

/** Compare a run against the manifest; returns the findings, empty when it matches. */
export function findings(manifest, counts) {
  const found = []
  for (const name of Object.keys(manifest).sort(byName)) {
    const actual = counts.get(name) ?? 0
    if (actual !== manifest[name]) {
      found.push(`${name}: ran ${actual} tests, manifest declares ${manifest[name]}`)
    }
  }
  for (const name of [...counts.keys()].sort(byName)) {
    if (!(name in manifest)) {
      found.push(`${name}: ran ${counts.get(name)} tests and is not in the manifest`)
    }
  }
  let total = 0
  for (const count of counts.values()) total += count
  if (total === 0) found.push('the run executed no tests at all')
  return found
}

export async function readManifest() {
  const manifest = JSON.parse(await fs.readFile(manifestPath, 'utf8'))
  const entries = Object.entries(manifest)
  if (entries.length === 0) throw new Error('spec-manifest.json declares no spec files')
  for (const [name, count] of entries) {
    if (!Number.isInteger(count) || count <= 0) {
      throw new Error(
        `spec-manifest.json declares ${name} as ${count}, expected a positive integer`,
      )
    }
  }
  return manifest
}

async function main(argv) {
  const reportPath = argv[2]
  if (!reportPath) {
    console.error('usage: node scripts/check-e2e-spec-counts.mjs <playwright-json-report>')
    return 2
  }
  const manifest = await readManifest()
  let report
  try {
    report = JSON.parse(await fs.readFile(resolve(reportPath), 'utf8'))
  } catch (error) {
    console.error(`FAIL: no readable Playwright JSON report at ${reportPath}: ${error.message}`)
    return 1
  }
  const counts = observedCounts(report)
  const found = findings(manifest, counts)
  if (found.length > 0) {
    console.error('FAIL: the Playwright run does not match the declared spec manifest')
    for (const finding of found) console.error(`  ${finding}`)
    console.error(
      '\nIf the change is deliberate, update apps/web/tests/e2e/spec-manifest.json in the same commit.',
    )
    return 1
  }
  const declared = Object.entries(manifest)
    .map(([name, count]) => `${name}=${count}`)
    .join(', ')
  let total = 0
  for (const count of counts.values()) total += count
  console.log(`e2e spec gate passed: ${total} tests across ${counts.size} files (${declared})`)
  return 0
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  process.exitCode = await main(process.argv)
}
