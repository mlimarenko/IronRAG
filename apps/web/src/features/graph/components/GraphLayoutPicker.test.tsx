import userEvent from '@testing-library/user-event'
import { render, screen } from '@testing-library/react'
import { describe, expect, it, vi } from 'vitest'

import i18n from '@/shared/i18n'

import { GraphLayoutPicker } from './GraphLayoutPicker'

describe('GraphLayoutPicker', () => {
  it('opens from the trigger with ArrowDown without claiming menu semantics', async () => {
    const user = userEvent.setup()
    render(
      <GraphLayoutPicker
        onChange={vi.fn()}
        recommended={null}
        t={i18n.t.bind(i18n)}
        value="force"
      />,
    )

    const trigger = screen.getByRole('button', { name: 'Layout' })
    trigger.focus()
    await user.keyboard('{ArrowDown}')

    expect(screen.getByRole('button', { name: 'Force' })).toHaveAttribute('aria-pressed', 'true')
    expect(screen.queryByRole('menu')).not.toBeInTheDocument()
    expect(screen.queryByRole('listbox')).not.toBeInTheDocument()
  })

  it('uses native buttons for the layout picker and its options', async () => {
    const user = userEvent.setup()
    const onChange = vi.fn()
    render(
      <GraphLayoutPicker
        onChange={onChange}
        recommended="bands"
        t={i18n.t.bind(i18n)}
        value="bands"
      />,
    )

    const trigger = screen.getByRole('button', { name: 'Layout' })

    expect(trigger).not.toHaveAttribute('role')
    expect(trigger).not.toHaveAttribute('aria-haspopup')

    await user.click(trigger)

    const activeOption = screen.getByRole('button', { name: /^Bands/ })
    expect(activeOption).toHaveAttribute('aria-pressed', 'true')
    expect(activeOption).not.toHaveAttribute('role')

    const nextOption = screen.getByRole('button', { name: 'Force' })
    await user.click(nextOption)

    expect(onChange).toHaveBeenCalledWith('force')
    expect(screen.queryByRole('button', { name: 'Force' })).not.toBeInTheDocument()
    expect(trigger).toHaveFocus()
  })
})
