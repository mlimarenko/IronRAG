import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { render, screen } from '@testing-library/react'
import i18n from 'i18next'
import type { ReactNode } from 'react'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import { DebugPanel } from './DebugPanel'

const mocks = vi.hoisted(() => ({
  getExecution: vi.fn(),
  getSystemPrompt: vi.fn(),
}))

vi.mock('@/shared/api', () => ({
  queries: {
    getAssistantSystemPromptOptions: () => ({
      queryFn: mocks.getSystemPrompt,
      queryKey: ['assistant-system-prompt'],
    }),
    getQueryExecutionOptions: ({ path }: { path: { executionId: string } }) => ({
      queryFn: () => mocks.getExecution(path.executionId),
      queryKey: ['assistant-execution', path.executionId],
    }),
  },
}))

function renderDebugPanel(executionId: string | null) {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })

  function Wrapper({ children }: Readonly<{ children: ReactNode }>) {
    return <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
  }

  return render(
    <DebugPanel
      context={null}
      contextError={null}
      contextLoading={false}
      evidence={null}
      executionId={executionId}
      libraryId={undefined}
      onClose={vi.fn()}
      open
    />,
    { wrapper: Wrapper },
  )
}

describe('DebugPanel', () => {
  beforeEach(async () => {
    vi.clearAllMocks()
    await i18n.changeLanguage('ru')
    i18n.addResource(
      'ru',
      'translation',
      'assistant.runtimeStageLabels.extract_graph',
      'Извлечение графа',
    )
    mocks.getExecution.mockResolvedValue({ runtimeStageSummaries: [] })
    mocks.getSystemPrompt.mockResolvedValue({ rendered: 'Prompt' })
  })

  it('shows one stacked empty state when no execution is selected', () => {
    const { container } = renderDebugPanel(null)

    expect(container.querySelector('.state-card--stacked .lucide-inbox')).not.toBeNull()
    expect(screen.queryByText(/Телеметрия этапов не записана/)).not.toBeInTheDocument()
    expect(screen.getAllByText('Нет выбранного выполнения')).toHaveLength(1)
  })

  it('uses the localized runtime stage instead of the backend enum label', async () => {
    mocks.getExecution.mockResolvedValue({
      runtimeStageSummaries: [
        { durationMs: 240, stageKind: 'extract_graph', stageLabel: 'Graph extraction' },
      ],
    })

    renderDebugPanel('execution-1')

    expect(await screen.findByText('Извлечение графа')).toBeInTheDocument()
    expect(screen.queryByText('Graph extraction')).not.toBeInTheDocument()
  })
})
