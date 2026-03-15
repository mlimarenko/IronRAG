import { describe, expect, it } from 'vitest'

import { shellNavItems } from '../shellNavigation'

describe('shellNavigation two-page contract', () => {
  it('keeps exactly two primary destinations', () => {
    const primaryItems = shellNavItems.filter((item) => item.stage === 'primary')

    expect(primaryItems).toHaveLength(2)
    expect(primaryItems.map((item) => item.key)).toEqual(['documents', 'ask'])
    expect(primaryItems.map((item) => item.to)).toEqual(['/documents', '/search'])
  })

  it('keeps advanced destinations secondary', () => {
    const advancedItems = shellNavItems.filter((item) => item.stage === 'advanced')

    expect(advancedItems).toHaveLength(1)
    expect(advancedItems[0]).toMatchObject({
      key: 'advanced',
      to: '/advanced/context',
      emphasis: 'secondary',
    })
  })
})
