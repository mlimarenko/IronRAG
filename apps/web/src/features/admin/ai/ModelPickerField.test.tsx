import { render, screen, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'

import { ModelPickerField } from './ModelPickerField'
import type { ModelPickerOption } from '../model/modelPicker'

vi.mock('react-i18next', () => ({
  useTranslation: () => ({ t: (key: string) => key, i18n: { language: 'en' } }),
}))

function option(overrides: Partial<ModelPickerOption> = {}): ModelPickerOption {
  return {
    key: 'vendor/one',
    modelName: 'vendor/one',
    selectable: true,
    inputModalities: ['text'],
    ...overrides,
  }
}

const options: ModelPickerOption[] = [
  option({
    key: 'a',
    modelName: 'vendor/cheap',
    contextWindow: 262_144,
    maxOutputTokens: 8_192,
    price: { currencyCode: 'USD', input: '0.4000000', output: '1.6000000' },
  }),
  option({
    key: 'b',
    modelName: 'vendor/vision',
    inputModalities: ['text', 'image'],
    modelCatalogId: 'model-b',
  }),
  option({
    key: 'c',
    modelName: 'vendor/blocked',
    selectable: false,
    note: 'cannot_serve',
  }),
]

describe('ModelPickerField', () => {
  it('shows what a choice costs and how much it holds', () => {
    render(<ModelPickerField options={options} value="" loading={false} onChange={vi.fn()} />)

    const row = screen.getByRole('row', { name: /vendor\/cheap/ })
    // 262144 tokens is unreadable; 262K is the number a person compares.
    expect(within(row).getByText('262K')).toBeInTheDocument()
    expect(within(row).getByText('8K')).toBeInTheDocument()
    // A decimal column pads with zeros that say nothing about the price.
    expect(within(row).getByText('0.4')).toBeInTheDocument()
    expect(within(row).getByText('1.6')).toBeInTheDocument()
    expect(within(row).getByText('USD')).toBeInTheDocument()
  })

  it('says nothing rather than zero where the provider published no price', () => {
    render(<ModelPickerField options={options} value="" loading={false} onChange={vi.fn()} />)

    const row = screen.getByRole('row', { name: /vendor\/vision/ })
    expect(within(row).getAllByText('—').length).toBeGreaterThan(0)
  })

  it('narrows the list by name', async () => {
    const user = userEvent.setup()
    render(<ModelPickerField options={options} value="" loading={false} onChange={vi.fn()} />)

    await user.type(screen.getByRole('searchbox'), 'vision')

    const table = screen.getByRole('table', { name: 'admin.model' })
    expect(within(table).getAllByRole('button', { pressed: false })).toHaveLength(1)
    expect(screen.getByRole('button', { name: /vendor\/vision/ })).toBeInTheDocument()
  })

  it('narrows the list to the models that can take the job', async () => {
    const user = userEvent.setup()
    render(<ModelPickerField options={options} value="" loading={false} onChange={vi.fn()} />)

    await user.click(screen.getByRole('button', { name: /facets.serves/ }))

    const table = screen.getByRole('table', { name: 'admin.model' })
    const names = within(table)
      .getAllByRole('row')
      .slice(1)
      .map((row) => row.textContent ?? '')
    expect(names.some((name) => name.includes('vendor/blocked'))).toBe(false)
    expect(names).toHaveLength(2)
  })

  it('keeps a model it cannot offer visible, with the reason, and refuses the click', async () => {
    // Hiding these is what produced an empty list that explained nothing.
    const user = userEvent.setup()
    const onChange = vi.fn()
    render(<ModelPickerField options={options} value="" loading={false} onChange={onChange} />)

    await user.click(screen.getByRole('button', { name: /facets.all/ }))
    const row = screen.getByRole('row', { name: /vendor\/blocked/ })
    expect(within(row).getByText('admin.aiPanel.modelNotes.cannot_serve')).toBeInTheDocument()

    await user.click(within(row).getByRole('button'))

    expect(onChange).not.toHaveBeenCalled()
  })

  it('reports the chosen model to the form', async () => {
    const user = userEvent.setup()
    const onChange = vi.fn()
    render(<ModelPickerField options={options} value="" loading={false} onChange={onChange} />)

    await user.click(screen.getByRole('button', { name: /vendor\/cheap/ }))

    expect(onChange).toHaveBeenCalledWith('a')
  })

  it('filters output prices independently when input prices are identical', async () => {
    const user = userEvent.setup()
    render(
      <ModelPickerField
        options={[
          options[0]!,
          option({
            key: 'other',
            modelName: 'vendor/other',
            price: { currencyCode: 'EUR', input: '0.4', output: '2.4' },
          }),
        ]}
        value=""
        loading={false}
        onChange={vi.fn()}
      />,
    )
    const header = document.querySelector('th[data-column="outputPrice"]') as HTMLElement
    await user.click(within(header).getByRole('button'))
    await user.click(screen.getByRole('menuitemcheckbox', { name: /^2\.4/ }))
    await user.keyboard('{Escape}')
    expect(screen.getByText('vendor/other')).toBeVisible()
    expect(screen.queryByText('vendor/cheap')).toBeNull()
  })

  it.each(['1.6', 'USD', '262144', 'modelNotes.cannot_serve'])(
    'searches all displayed fields, not only the model name: %s',
    async (search) => {
      render(<ModelPickerField options={options} value="" loading={false} onChange={vi.fn()} />)
      if (search.includes('cannot_serve')) {
        await userEvent.setup().click(screen.getByRole('button', { name: /facets.all/ }))
      }
      await userEvent.setup().type(screen.getByRole('searchbox'), search)
      const names = within(screen.getByRole('table'))
        .getAllByRole('row')
        .slice(1)
        .map((row) => row.textContent)
      expect(names).toHaveLength(1)
      expect(names[0]).toContain(
        search.includes('cannot_serve') ? 'vendor/blocked' : 'vendor/cheap',
      )
    },
  )
  it('defaults to compatible models and retains an explicit all-models option', async () => {
    render(<ModelPickerField options={options} value="" loading={false} onChange={vi.fn()} />)
    expect(screen.getByRole('button', { name: /facets.serves/ })).toHaveAttribute(
      'aria-pressed',
      'true',
    )
    expect(screen.queryByRole('row', { name: /vendor\/blocked/ })).not.toBeInTheDocument()
    await userEvent.setup().click(screen.getByRole('button', { name: /facets.all/ }))
    expect(screen.getByRole('row', { name: /vendor\/blocked/ })).toBeInTheDocument()
  })

  it('keeps the selected-model summary visible when search hides its row', async () => {
    render(<ModelPickerField options={options} value="a" loading={false} onChange={vi.fn()} />)
    await userEvent.setup().type(screen.getByRole('searchbox'), 'vision')
    const selection = screen.getByRole('status', { name: 'admin.model' })
    expect(selection.tagName).toBe('OUTPUT')
    expect(within(selection).getByText('vendor/cheap')).toBeVisible()
    expect(screen.queryByRole('row', { name: /vendor\/cheap/ })).not.toBeInTheDocument()
  })

  it('omits comparison columns with no published metadata and explains all incompatible choices', () => {
    render(<ModelPickerField options={[options[2]!]} value="" loading={false} onChange={vi.fn()} />)
    expect(screen.getByRole('row', { name: /vendor\/blocked/ })).toHaveTextContent(
      'modelNotes.cannot_serve',
    )
    expect(screen.queryByRole('columnheader', { name: /columns.context/ })).not.toBeInTheDocument()
    expect(
      screen.queryByRole('columnheader', { name: /columns.inputPrice/ }),
    ).not.toBeInTheDocument()
  })

  it('starts with ten rows while search still reaches the complete list', async () => {
    const many = Array.from({ length: 30 }, (_, index) =>
      option({ key: `${index}`, modelName: `model-${index}` }),
    )
    render(<ModelPickerField options={many} value="" loading={false} onChange={vi.fn()} />)
    expect(within(screen.getByRole('table')).getAllByRole('row')).toHaveLength(11)
    await userEvent.setup().type(screen.getByRole('searchbox'), 'model-29')
    expect(screen.getByRole('row', { name: /model-29/ })).toBeInTheDocument()
  })
})
