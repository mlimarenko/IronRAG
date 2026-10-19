import { promises as fs } from 'node:fs'
import { basename } from 'node:path'
import { describe, expect, it } from 'vitest'

import { findings, observedCounts, readManifest, specDir } from './check-e2e-spec-counts.mjs'

/** A Playwright report where each named file contributes N single-run specs. */
function report(...files) {
  return {
    suites: files.map(([file, count]) => ({
      file,
      specs: Array.from({ length: count }, (_, index) => ({
        title: `spec ${index}`,
        tests: [{ status: 'expected' }],
      })),
    })),
  }
}

describe('e2e spec gate', () => {
  it('passes when the run matches the manifest', () => {
    const counts = observedCounts(report(['a.spec.ts', 3], ['b.spec.ts', 4]))
    expect(findings({ 'a.spec.ts': 3, 'b.spec.ts': 4 }, counts)).toEqual([])
  })

  it('fails when a declared file ran nothing', () => {
    const counts = observedCounts(report(['a.spec.ts', 3]))
    expect(findings({ 'a.spec.ts': 3, 'b.spec.ts': 4 }, counts)).toEqual([
      'b.spec.ts: ran 0 tests, manifest declares 4',
    ])
  })

  it('fails when a file lost a test', () => {
    const counts = observedCounts(report(['a.spec.ts', 2]))
    expect(findings({ 'a.spec.ts': 3 }, counts)).toEqual([
      'a.spec.ts: ran 2 tests, manifest declares 3',
    ])
  })

  it('fails on a spec file the manifest does not declare', () => {
    // A new spec must be declared in the same commit; otherwise the next
    // rename can silently drop it again.
    const counts = observedCounts(report(['a.spec.ts', 3], ['c.spec.ts', 1]))
    expect(findings({ 'a.spec.ts': 3 }, counts)).toEqual([
      'c.spec.ts: ran 1 tests and is not in the manifest',
    ])
  })

  it('fails on a run that executed nothing', () => {
    expect(findings({ 'a.spec.ts': 3 }, observedCounts({ suites: [] }))).toEqual([
      'a.spec.ts: ran 0 tests, manifest declares 3',
      'the run executed no tests at all',
    ])
  })

  it('counts tests nested in describe blocks', () => {
    const counts = observedCounts({
      suites: [
        {
          file: 'a.spec.ts',
          specs: [{ title: 'top', tests: [{ status: 'expected' }] }],
          suites: [
            { title: 'group', specs: [{ title: 'inner', tests: [{ status: 'expected' }] }] },
          ],
        },
      ],
    })
    expect(counts.get('a.spec.ts')).toBe(2)
  })

  it('counts each retry of one spec once', () => {
    // Playwright reports retries as extra entries under the same spec; the
    // manifest counts specs, so a flaky run must not read as extra coverage.
    const counts = observedCounts({
      suites: [
        {
          file: 'a.spec.ts',
          specs: [{ title: 'retried', tests: [{ status: 'expected' }] }],
        },
      ],
    })
    expect(counts.get('a.spec.ts')).toBe(1)
  })

  it('declares every spec file that exists on disk', async () => {
    const manifest = await readManifest()
    const byName = (a, b) => a.localeCompare(b)
    const onDisk = (await fs.readdir(specDir))
      .filter((name) => name.endsWith('.spec.ts'))
      .map((name) => basename(name))
    expect(Object.keys(manifest).sort(byName)).toEqual(onDisk.sort(byName))
  })
})
