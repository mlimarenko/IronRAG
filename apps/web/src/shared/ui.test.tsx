import { render, screen } from '@testing-library/react'
import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { describe, expect, it, vi } from 'vitest'
import { useState } from 'react'
import userEvent from '@testing-library/user-event'

import {
  Badge,
  Button,
  Modal,
  PageHeader,
  Panel,
  Progress,
  SettingsList,
  SettingsListItem,
  Tabs,
} from './ui'

const componentsStyles = readFileSync(resolve(process.cwd(), 'src/styles/components.css'), 'utf8')

function ExampleTabs() {
  const [value, setValue] = useState('first')
  return (
    <Tabs
      id="example"
      label="Sections"
      value={value}
      onChange={setValue}
      items={[
        { value: 'first', label: 'First', suffix: <Badge>8</Badge> },
        { value: 'second', label: 'Second' },
      ]}
    >
      <p>{value} content</p>
    </Tabs>
  )
}

describe('Shared tabs', () => {
  it('links every tab to a panel and keeps counts outside the accessible name', () => {
    render(<ExampleTabs />)
    const first = screen.getByRole('tab', { name: 'First' })
    const second = screen.getByRole('tab', { name: 'Second' })
    expect(first).toHaveAttribute('aria-selected', 'true')
    expect(first).toHaveAttribute('tabindex', '0')
    expect(second).toHaveAttribute('tabindex', '-1')
    for (const tab of [first, second]) {
      expect(document.getElementById(tab.getAttribute('aria-controls') ?? '')).not.toBeNull()
    }
    expect(screen.getByRole('tabpanel', { name: 'First' })).toHaveTextContent('first content')
    expect(screen.queryByText('second content')).not.toBeInTheDocument()
  })

  it('supports roving focus, arrow wrapping and Home/End activation', async () => {
    const user = userEvent.setup()
    render(<ExampleTabs />)
    const first = screen.getByRole('tab', { name: 'First' })
    const second = screen.getByRole('tab', { name: 'Second' })
    await user.tab()
    expect(first).toHaveFocus()
    await user.keyboard('{ArrowRight}')
    expect(second).toHaveFocus()
    expect(screen.getByRole('tabpanel', { name: 'Second' })).toHaveTextContent('second content')
    await user.keyboard('{ArrowRight}')
    expect(first).toHaveFocus()
    await user.keyboard('{End}')
    expect(second).toHaveFocus()
    await user.keyboard('{Home}')
    expect(first).toHaveFocus()
    await user.keyboard('{ArrowLeft}')
    expect(second).toHaveFocus()
  })
})

describe('Semantic progress', () => {
  it('uses a bounded native indicator and the supplied status tone', () => {
    const { rerender } = render(<Progress value={140} label="Readiness" tone="ready" />)
    const progress = screen.getByRole('progressbar', { name: 'Readiness' })
    expect(progress.tagName).toBe('PROGRESS')
    expect(progress).toHaveAttribute('value', '100')
    expect(progress).toHaveAttribute('max', '100')
    expect(progress).toHaveAttribute('aria-valuenow', '100')
    expect(progress).toHaveClass('progress--ready')
    rerender(<Progress value={-5} label="Readiness" />)
    expect(progress).toHaveAttribute('value', '0')
    expect(progress).toHaveClass('progress--accent')
  })
})

describe('Shared presentation primitives', () => {
  it('links each compact settings disclosure to its own details and keeps actions independent', async () => {
    const user = userEvent.setup()
    const edit = vi.fn()
    render(
      <SettingsList label="Example settings">
        <SettingsListItem
          title="First setting"
          description="Inherited"
          summary="Current value"
          actions={<Button onClick={edit}>Edit</Button>}
        >
          <p>Additional information</p>
          <Button>Delete</Button>
        </SettingsListItem>
        <SettingsListItem title="Second setting">
          <p>Other details</p>
        </SettingsListItem>
      </SettingsList>,
    )
    const first = screen.getByRole('button', { name: 'First setting' })
    const second = screen.getByRole('button', { name: 'Second setting' })
    expect(first).toHaveAttribute('aria-expanded', 'false')
    expect(first.getAttribute('aria-controls')).not.toBe(second.getAttribute('aria-controls'))
    expect(document.getElementById(first.getAttribute('aria-controls') ?? '')).toHaveAttribute(
      'hidden',
    )
    expect(screen.queryByRole('button', { name: 'Delete' })).not.toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'Edit' }))
    expect(edit).toHaveBeenCalledOnce()
    expect(first).toHaveAttribute('aria-expanded', 'false')
    first.focus()
    await user.keyboard('{Enter}')
    expect(first).toHaveAttribute('aria-expanded', 'true')
    expect(screen.getByRole('button', { name: 'Delete' })).toBeVisible()
    expect(second).toHaveAttribute('aria-expanded', 'false')
    await user.keyboard(' ')
    expect(first).toHaveAttribute('aria-expanded', 'false')
    expect(first).toHaveFocus()
  })

  it('renders a single page heading without an empty eyebrow region', () => {
    render(<PageHeader title="Documents" actions={<Button>Upload</Button>} />)

    expect(screen.getAllByRole('heading', { level: 1 })).toHaveLength(1)
    expect(document.querySelector('.eyebrow')).toBeNull()
    expect(screen.getByRole('button', { name: 'Upload' })).toHaveAttribute('type', 'button')
  })

  it('keeps the supplied description and actions in their semantic slots', () => {
    render(
      <Panel className="example-panel">
        <PageHeader eyebrow="Library" title="Documents" description="Upload and manage files" />
        <Button variant="primary" type="submit" disabled>
          Save
        </Button>
      </Panel>,
    )

    expect(screen.getByText('Library')).toHaveClass('eyebrow')
    expect(screen.getByText('Upload and manage files')).toBeVisible()
    expect(document.querySelector('[data-slot="card"]')).toHaveClass('example-panel')
    expect(screen.getByRole('button', { name: 'Save' })).toHaveAttribute('data-slot', 'button')
    expect(screen.getByRole('button', { name: 'Save' })).toHaveAttribute('type', 'submit')
    expect(screen.getByRole('button', { name: 'Save' })).toBeDisabled()
  })
})

// Guards the shared Modal shell contract (footer lives outside the scrolling
// body), not the IRG-129 migration: it passes before and after. The migration's
// RED proof is BindingsSection.footer.test.tsx and dialog-actions.spec.ts.
describe('Modal footer contract', () => {
  it('renders the action slot inside .dialog-footer and outside .dialog-body', () => {
    render(
      <Modal
        open
        onOpenChange={vi.fn()}
        title="Example dialog"
        footer={<button type="button">Act</button>}
      >
        <p>Body</p>
      </Modal>,
    )

    const dialog = screen.getByRole('dialog')
    const footer = dialog.querySelector<HTMLElement>('.dialog-footer')
    const body = dialog.querySelector<HTMLElement>('.dialog-body')
    expect(footer).not.toBeNull()
    expect(body).not.toBeNull()

    const action = screen.getByRole('button', { name: 'Act' })
    expect(footer).toContainElement(action)
    expect(body).not.toContainElement(action)
  })

  it('renders no footer region when no footer is passed', () => {
    render(
      <Modal open onOpenChange={vi.fn()} title="Example dialog">
        <p>Body</p>
      </Modal>,
    )

    expect(screen.getByRole('dialog').querySelector('.dialog-footer')).toBeNull()
  })
})

describe('Badge label clipping contract', () => {
  /**
   * jsdom resolves the cascade only for stylesheets it parses, so inject the
   * real `components.css`. The label used to be an anonymous flex item, whose
   * own `text-overflow` is `clip`, so a badge squeezed by a narrow column was
   * cut mid-glyph despite the ellipsis declared on the inline-flex badge.
   */
  it('ellipsizes a squeezed badge instead of cutting the label mid-glyph', () => {
    const style = document.createElement('style')
    style.textContent = componentsStyles
    document.head.append(style)

    try {
      render(<Badge tone="warning">Недоступно: провайдер отключён</Badge>)

      const badge = document.querySelector<HTMLElement>('.badge')
      const label = document.querySelector<HTMLElement>('.badge__label')
      expect(badge).not.toBeNull()
      expect(label).not.toBeNull()
      expect(badge).toContainElement(label)

      const labelStyle = getComputedStyle(label as HTMLElement)
      expect(labelStyle.textOverflow).toBe('ellipsis')
      expect(labelStyle.overflow).toBe('hidden')
      expect(labelStyle.minWidth).toBe('0px')
    } finally {
      style.remove()
    }
  })
})
