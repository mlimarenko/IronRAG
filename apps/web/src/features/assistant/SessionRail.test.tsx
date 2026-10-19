import { render, screen } from '@testing-library/react'
import i18n from 'i18next'
import type { ComponentProps } from 'react'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import { SessionRail } from './SessionRail'

const mocks = vi.hoisted(() => ({
  preferences: {
    sessionRailCollapsed: false,
    setSessionRailCollapsed: vi.fn(),
  },
}))

vi.mock('@/app/preferences-context', () => ({
  usePreferences: () => mocks.preferences,
}))

const defaultProps = {
  activeId: null,
  canManage: true,
  disabled: false,
  error: false,
  loading: false,
  locale: 'ru',
  onDelete: vi.fn(),
  onNew: vi.fn(),
  onRename: vi.fn(),
  onRetry: vi.fn(),
  onSearchChange: vi.fn(),
  onSelect: vi.fn(),
  search: '',
  sessions: [],
} satisfies ComponentProps<typeof SessionRail>

describe('SessionRail', () => {
  beforeEach(async () => {
    vi.clearAllMocks()
    mocks.preferences.sessionRailCollapsed = false
    await i18n.changeLanguage('ru')
    i18n.addResource(
      'ru',
      'translation',
      'assistant.sessionActionsDisabled',
      'Дождитесь завершения ответа или текущего действия с сессией.',
    )
  })

  it('renders an empty session list as a stacked state with a next-step hint', () => {
    const { container } = render(<SessionRail {...defaultProps} />)

    expect(container.querySelector('.state-card--stacked .lucide-inbox')).not.toBeNull()
    expect(screen.getByText('Начните новый диалог для вопросов.')).toBeInTheDocument()
  })

  it('explains how to recover when the session search has no matches', () => {
    render(
      <SessionRail
        {...defaultProps}
        search="missing"
        sessions={[
          {
            id: 'session-1',
            libraryId: 'library-1',
            title: 'Reference session',
            turnCount: 1,
            updatedAt: '2026-08-11T10:00:00Z',
          },
        ]}
      />,
    )

    expect(screen.getByText('Попробуйте изменить запрос.')).toBeInTheDocument()
  })

  it('associates disabled session controls with a visible explanation', () => {
    render(<SessionRail {...defaultProps} disabled />)

    const explanation = screen.getByText(
      'Дождитесь завершения ответа или текущего действия с сессией.',
    )
    const newSession = screen.getByRole('button', { name: 'Новая сессия' })

    expect(explanation.id).toMatch(/session-actions-disabled$/)
    expect(newSession).toHaveAttribute('aria-describedby', explanation.id)
    expect(newSession).toHaveAttribute('title', explanation.textContent)
  })
})
