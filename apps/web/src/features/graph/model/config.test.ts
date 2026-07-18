import { describe, expect, it } from 'vitest'

import i18n from '@/shared/i18n'
import {
  DEFAULT_GRAPH_LAYOUT,
  GRAPH_LAYOUT_OPTIONS,
  isGraphLayoutType,
  normalizeRecommendedGraphLayout,
} from './config'

describe('graph layout config', () => {
  it('keeps every configured layout typed and translated', () => {
    const ids = new Set<string>()

    for (const option of GRAPH_LAYOUT_OPTIONS) {
      expect(ids.has(option.id)).toBe(false)
      ids.add(option.id)
      expect(isGraphLayoutType(option.id)).toBe(true)

      for (const lng of ['en', 'ru']) {
        const label = i18n.t(option.labelKey, { lng })
        const description = i18n.t(option.descriptionKey, { lng })
        expect(label).not.toBe(option.labelKey)
        expect(label.trim().length).toBeGreaterThan(0)
        expect(description).not.toBe(option.descriptionKey)
        expect(description.trim().length).toBeGreaterThan(0)
      }
    }
  })

  it('normalizes the legacy bands recommendation to a modern default', () => {
    expect(DEFAULT_GRAPH_LAYOUT).not.toBe('bands')
    expect(normalizeRecommendedGraphLayout('bands')).toBe(DEFAULT_GRAPH_LAYOUT)
    expect(normalizeRecommendedGraphLayout('flow')).toBe('flow')
    expect(normalizeRecommendedGraphLayout('unknown')).toBeNull()
  })
})
