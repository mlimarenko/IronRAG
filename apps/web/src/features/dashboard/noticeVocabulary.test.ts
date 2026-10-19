import { describe, expect, it } from 'vitest'

import en from '@/shared/i18n/en.json'
import ru from '@/shared/i18n/ru.json'

const locales = { en, ru } as const

/// The dashboard can raise several notices at once — the API pushes the
/// graph and vector coverage gaps from the document counts while the failed
/// and stalled notices arrive from the operator warning feed, and it only
/// de-duplicates identical codes. Two notices that describe different
/// conditions therefore have to read differently, or the reader sees what looks
/// like the same sentence twice and cannot tell which one they already dealt
/// with.
describe('dashboard notice vocabulary', () => {
  it('gives every notice a title no other notice shares', () => {
    for (const [locale, bundle] of Object.entries(locales)) {
      const titles = Object.entries(bundle.dashboard.attentionItem).map(
        ([code, item]) => [code, item.title] as const,
      )
      const byTitle = new Map<string, string>()
      for (const [code, title] of titles) {
        const owner = byTitle.get(title)
        expect(owner, `${locale}: "${title}" is used by both ${owner} and ${code}`).toBeUndefined()
        byTitle.set(title, code)
      }
    }
  })

  it('states one condition in one wording across the panels that report it', () => {
    for (const [locale, bundle] of Object.entries(locales)) {
      // The readiness badge and the coverage notice report the same fact — ready
      // materials without relationships. Wording them differently made one
      // condition look like two problems.
      expect(bundle.dashboard.attentionItem.graph_coverage_gap.title, locale).toBe(
        bundle.dashboard.factMapIncomplete,
      )
    }
  })

  it('gives the graph and vector coverage gaps different wordings', () => {
    for (const [locale, bundle] of Object.entries(locales)) {
      const notices = bundle.dashboard.attentionItem
      // Missing relationships and missing vectors are two different holes with
      // different recoveries, so the two coverage notices must not read alike.
      expect(notices.graph_coverage_gap.title, locale).not.toBe(notices.vector_index_gap.title)
    }
  })
})
