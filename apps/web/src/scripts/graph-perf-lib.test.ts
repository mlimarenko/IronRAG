import { describe, expect, it } from 'vitest'

import {
  buildBrowserLaunchOptions,
  getJankTag,
  parseSessionCookie,
  summarizeCpuProfile,
} from '../../scripts/graph-perf-lib.mjs'

describe('graph performance helpers', () => {
  it('parses the first session cookie without a regular expression', () => {
    expect(parseSessionCookie('ironrag_session=encoded-value; Path=/; HttpOnly')).toEqual([
      'ironrag_session',
      'encoded-value',
    ])
  })

  it('rejects malformed session cookie headers', () => {
    expect(parseSessionCookie('missing-separator')).toBeNull()
    expect(parseSessionCookie('=missing-name; Path=/')).toBeNull()
  })

  it('labels only janky long tasks', () => {
    expect(getJankTag(80)).toBe('')
    expect(getJankTag(81)).toBe(' <- janky')
    expect(getJankTag(201)).toBe(' <== SEVERE')
  })

  it('summarizes CPU samples by function and sorts them by self time', () => {
    const profile = {
      nodes: [
        { id: 1, callFrame: { functionName: '(root)', url: '', lineNumber: 0 } },
        { id: 2, callFrame: { functionName: 'slow', url: 'a.js', lineNumber: 3 } },
        { id: 3, callFrame: { functionName: 'fast', url: 'b.js', lineNumber: 7 } },
      ],
      samples: [2, 3, 2, 2],
      timeDeltas: [1200, 300, 900, 500],
    }

    expect(summarizeCpuProfile(profile)).toEqual([
      ['slow @ a.js:3', 2600],
      ['fast @ b.js:7', 300],
    ])
  })

  it('uses GPU arguments only for Chromium', () => {
    expect(buildBrowserLaunchOptions('firefox', false, '--unused')).toEqual({ headless: false })
    expect(buildBrowserLaunchOptions('chromium', true, '--one --two')).toEqual({
      headless: true,
      args: ['--one', '--two'],
    })
  })
})
