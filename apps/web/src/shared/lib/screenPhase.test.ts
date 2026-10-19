import { describe, expect, test } from 'vitest'

import { screenPhase } from './screenPhase'

describe('screenPhase', () => {
  test('reports loading even when the previous attempt failed', () => {
    expect(screenPhase({ loading: true, failed: true, empty: true })).toBe('loading')
  })

  test('reports error ahead of empty so a failed load is not read as "no rows"', () => {
    expect(screenPhase({ loading: false, failed: true, empty: true })).toBe('error')
  })

  test('reports empty when the load succeeded with no rows', () => {
    expect(screenPhase({ loading: false, failed: false, empty: true })).toBe('empty')
  })

  test('reports ready when there is content', () => {
    expect(screenPhase({ loading: false, failed: false, empty: false })).toBe('ready')
  })

  test('treats an omitted emptiness signal as "there is content"', () => {
    expect(screenPhase({ loading: false, failed: false })).toBe('ready')
  })
})
