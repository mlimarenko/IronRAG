import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { useState } from 'react'
import { describe, expect, it } from 'vitest'
import type { GraphNode, GraphNodeType } from '@/shared/types'
import { EMPTY_LEGEND_FILTER, buildTypeLegend, type LegendFilter } from '../model/typeLegend'
import { GraphLegend } from './GraphLegend'

function node(id: string, type: GraphNodeType, subType?: string): GraphNode {
  return { id, label: id, type, properties: {}, edgeCount: 0, ...(subType ? { subType } : {}) }
}

const legend = buildTypeLegend([
  node('alice', 'person', 'engineer'),
  node('bob', 'person', 'engineer'),
  node('carol', 'person', 'manager'),
  node('acme', 'organization'),
])

function LegendHarness() {
  const [filter, setFilter] = useState<LegendFilter>(EMPTY_LEGEND_FILTER)
  return (
    <GraphLegend
      legend={legend}
      filter={filter}
      onFilterChange={setFilter}
      onShowAll={() => setFilter(EMPTY_LEGEND_FILTER)}
    />
  )
}

function typeButton(name: RegExp) {
  return screen.getByRole('button', { name })
}

describe('GraphLegend', () => {
  it('hides a type with its sub-types on a plain click', async () => {
    const user = userEvent.setup()
    render(<LegendHarness />)

    await user.click(typeButton(/^Person/))

    expect(typeButton(/^Person/)).toHaveAttribute('aria-pressed', 'false')
    expect(screen.queryByRole('button', { name: /Engineer/ })).not.toBeInTheDocument()
    expect(typeButton(/^Organization/)).toHaveAttribute('aria-pressed', 'true')
  })

  it('hides only the clicked sub-type', async () => {
    const user = userEvent.setup()
    render(<LegendHarness />)

    await user.click(screen.getByRole('button', { name: /Engineer/ }))

    expect(screen.getByRole('button', { name: /Engineer/ })).toHaveAttribute(
      'aria-pressed',
      'false',
    )
    expect(screen.getByRole('button', { name: /Manager/ })).toHaveAttribute('aria-pressed', 'true')
    expect(typeButton(/^Person/)).toHaveAttribute('aria-pressed', 'true')
  })

  it('shows only the clicked type while Ctrl is held, and everything on a repeat', async () => {
    const user = userEvent.setup()
    render(<LegendHarness />)

    await user.keyboard('{Control>}')
    await user.click(typeButton(/^Organization/))
    await user.keyboard('{/Control}')

    expect(typeButton(/^Organization/)).toHaveAttribute('aria-pressed', 'true')
    expect(typeButton(/^Person/)).toHaveAttribute('aria-pressed', 'false')

    await user.keyboard('{Control>}')
    await user.click(typeButton(/^Organization/))
    await user.keyboard('{/Control}')

    expect(typeButton(/^Person/)).toHaveAttribute('aria-pressed', 'true')
  })

  it('shows only the clicked sub-type while ⌘ is held', async () => {
    const user = userEvent.setup()
    render(<LegendHarness />)

    await user.keyboard('{Meta>}')
    await user.click(screen.getByRole('button', { name: /Manager/ }))
    await user.keyboard('{/Meta}')

    expect(screen.getByRole('button', { name: /Manager/ })).toHaveAttribute('aria-pressed', 'true')
    expect(screen.getByRole('button', { name: /Engineer/ })).toHaveAttribute(
      'aria-pressed',
      'false',
    )
    expect(typeButton(/^Organization/)).toHaveAttribute('aria-pressed', 'false')
  })
})
