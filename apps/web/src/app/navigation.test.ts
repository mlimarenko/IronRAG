import { describe, expect, it } from 'vitest'

import en from '@/shared/i18n/en.json'
import ru from '@/shared/i18n/ru.json'
import { adminNav, mainNav } from './navigation'

/// Every sidebar entry and the heading of the screen it opens must be the same
/// words. When they were not, the sidebar said "Overview" while the screen it
/// opened said "Memory for AI", and a reader had no way to tell they were the
/// same place. The map is the contract; a new destination that is not listed
/// here fails the coverage assertion below rather than silently opting out.
const NAV_LABEL_TO_SCREEN_HEADING: Record<string, string> = {
  'nav.dashboard': 'dashboard.eyebrow',
  'nav.documents': 'documents.title',
  'nav.graph': 'graph.title',
  'nav.assistant': 'assistant.title',
  'nav.libraries': 'admin.librariesTitle',
  'nav.audit': 'admin.auditTitle',
  'nav.ai': 'admin.aiTitle',
  'nav.access': 'admin.accessTitle',
  'nav.users': 'admin.usersTitle',
  'nav.webhooks': 'admin.webhooksTitle',
  'nav.system': 'admin.systemTitle',
}

const locales = { en, ru } as const

function lookup(bundle: unknown, dottedKey: string): string {
  const value = dottedKey
    .split('.')
    .reduce<unknown>(
      (node, part) =>
        typeof node === 'object' && node !== null
          ? (node as Record<string, unknown>)[part]
          : undefined,
      bundle,
    )
  expect(typeof value, `missing translation for ${dottedKey}`).toBe('string')
  return value as string
}

const navEntries = [...mainNav, ...adminNav.flatMap(([, entries]) => entries)]

describe('navigation vocabulary', () => {
  it('opens the processing queue from libraries, not a standalone sidebar entry', () => {
    expect(navEntries.map(([route]) => route)).not.toContain('/admin/queue')
    expect(navEntries.map(([route]) => route)).toContain('/admin/libraries')
  })
  it('names every destination in the sidebar exactly as its screen heading names it', () => {
    for (const [locale, bundle] of Object.entries(locales)) {
      for (const [navKey, headingKey] of Object.entries(NAV_LABEL_TO_SCREEN_HEADING)) {
        expect(
          lookup(bundle, navKey),
          `${locale}: ${navKey} must read the same as ${headingKey}`,
        ).toBe(lookup(bundle, headingKey))
      }
    }
  })

  it('holds every routed sidebar entry to that contract', () => {
    const mapped = Object.keys(NAV_LABEL_TO_SCREEN_HEADING).sort()
    const rendered = navEntries.map(([, labelKey]) => labelKey).sort()
    expect(rendered).toEqual(mapped)
  })

  it('routes every sidebar entry to a distinct path and gives it a distinct icon', () => {
    const routes = navEntries.map(([route]) => route)
    expect(new Set(routes).size).toBe(routes.length)
    // Two entries sharing an icon read as the same destination in the
    // collapsed sidebar, where the icon is all that is left of the label.
    const icons = navEntries.map(([, , icon]) => icon)
    expect(new Set(icons).size).toBe(icons.length)
  })

  it('groups the administration entries under headings that are translated in both locales', () => {
    expect(adminNav.length).toBeGreaterThan(1)
    for (const [locale, bundle] of Object.entries(locales)) {
      for (const [groupLabelKey, entries] of adminNav) {
        expect(lookup(bundle, groupLabelKey).length, `${locale}: ${groupLabelKey}`).toBeGreaterThan(
          0,
        )
        expect(entries.length, `${groupLabelKey} must not be an empty group`).toBeGreaterThan(0)
      }
    }
  })
})
