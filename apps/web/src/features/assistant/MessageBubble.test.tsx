import { render, screen, within } from '@testing-library/react'
import { describe, expect, it, vi } from 'vitest'

import i18n from '@/shared/i18n'

import type { AssistantMessage } from '@/shared/types'
import { MessageBubble } from './MessageBubble'

vi.mock('@/shared/contexts/app-context', () => ({
  useApp: () => ({ locale: 'en', user: { displayName: 'Ada Lovelace', login: 'ada' } }),
}))

/**
 * An answer a grounded assistant actually produces: a GFM table. Without the
 * GFM plugin the pipes reach the screen as literal text, so the operator reads
 * `| Этап | … |` instead of a table.
 */
const TABLE_ANSWER = ['| Этап | Результат |', '| --- | --- |', '| Разбор | Готово |'].join('\n')

function assistantMessage(content: string): AssistantMessage {
  return {
    id: 'message-1',
    role: 'assistant',
    content,
    timestamp: '2026-01-01T10:00:00Z',
  }
}

function renderBubble(content: string) {
  return render(
    <MessageBubble
      message={assistantMessage(content)}
      sourceCount={0}
      onClarify={vi.fn()}
      onSources={vi.fn()}
    />,
  )
}

describe('MessageBubble markdown', () => {
  it('renders a GFM table as a real table instead of raw pipes', () => {
    const { container } = renderBubble(TABLE_ANSWER)

    const table = screen.getByRole('table')
    expect(
      within(table)
        .getAllByRole('columnheader')
        .map((cell) => cell.textContent),
    ).toEqual(['Этап', 'Результат'])
    expect(
      within(table)
        .getAllByRole('cell')
        .map((cell) => cell.textContent),
    ).toEqual(['Разбор', 'Готово'])
    expect(container.textContent).not.toContain('|')
  })

  it('renders the error variant as a table too', () => {
    render(
      <MessageBubble
        isError
        message={assistantMessage(TABLE_ANSWER)}
        sourceCount={0}
        onClarify={vi.fn()}
        onSources={vi.fn()}
      />,
    )

    expect(screen.getByRole('table')).toBeInTheDocument()
  })
})

describe('MessageBubble failed turn', () => {
  it('keeps a single alert surface and offers retry inside it', () => {
    const onRetry = vi.fn()
    render(
      <MessageBubble
        isError
        message={assistantMessage('Извините, ассистент не смог выполнить запрос.')}
        sourceCount={0}
        onClarify={vi.fn()}
        onSources={vi.fn()}
        onRetry={onRetry}
      />,
    )

    const alerts = screen.getAllByRole('alert')
    expect(alerts).toHaveLength(1)
    const retry = within(alerts[0] as HTMLElement).getByRole('button', {
      name: i18n.t('assistant.retryAction'),
    })
    retry.click()
    expect(onRetry).toHaveBeenCalledTimes(1)
  })

  it('does not invent a retry control without a handler', () => {
    renderBubble('Извините, ассистент не смог выполнить запрос.')

    expect(screen.queryByRole('button', { name: i18n.t('assistant.retryAction') })).toBeNull()
  })
})
