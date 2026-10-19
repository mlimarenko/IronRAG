import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { describe, expect, it } from 'vitest'

describe('compact page typography', () => {
  it.each([
    'dashboard',
    'documents',
    'graph',
    'assistant',
    'admin',
    'misc',
    'components',
    'layout',
    'responsive',
    'data-grid',
  ])('%s keeps secondary text readable at the browser root size', (page) => {
    const styles = readFileSync(resolve(process.cwd(), `src/styles/${page}.css`), 'utf8')
    const sizes = [...styles.matchAll(/font-size:\s*([\d.]+)(rem|px)/gu)]
    expect(sizes.length).toBeGreaterThan(0)
    for (const [, value, unit] of sizes) {
      expect(Number(value), `${page}: ${value}${unit}`).toBeGreaterThanOrEqual(
        unit === 'rem' ? 0.75 : 12,
      )
    }
    // Custom font-size variables are declarations too, not an escape from the floor.
    for (const [, value, unit] of styles.matchAll(/--fs-[\w-]+:\s*([\d.]+)(rem|px)/gu)) {
      expect(Number(value), `${page}: ${value}${unit}`).toBeGreaterThanOrEqual(
        unit === 'rem' ? 0.75 : 12,
      )
    }
  })

  it('styles the single-column login and setup h1 without retired story selectors', () => {
    const styles = readFileSync(resolve(process.cwd(), 'src/styles/misc.css'), 'utf8')
    const layout = styles.match(/\.login-page\s*\{([^}]+)\}/u)?.[1]
    expect(layout).toContain('place-items: center')
    expect(layout).toContain('grid-template-columns: minmax(0, 1fr)')
    expect(styles).toContain('.login-form h1,')
    expect(styles).toContain('.bootstrap-flow > h1')
    expect(styles).not.toMatch(/\.login-(?:story|copy|feature)/u)
    expect(styles).not.toMatch(/\.(?:login-form|bootstrap-flow)\s*>?\s*h2/u)
  })
})
