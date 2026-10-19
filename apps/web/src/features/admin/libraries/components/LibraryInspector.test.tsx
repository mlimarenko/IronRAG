import { render, screen, within } from '@testing-library/react'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import type { AdminLibraryRow } from '@/features/admin/libraries/model/librariesAdapter'
import { LibraryInspector } from './LibraryInspector'

const mocks = vi.hoisted(() => {
  const translations: Record<string, string> = {
    'admin.enumLabels.libraryLifecycle.active': 'Активна',
    'admin.enumLabels.libraryLifecycle.archived': 'В архиве',
    'admin.libraries.activeLifecycle': 'Активные',
    'admin.libraries.health.failedDocuments': 'Документы с ошибками',
    'admin.libraries.health.lastSuccessfulSync': 'Последняя синхронизация',
    'admin.libraries.health.pendingJobs': 'Активные задачи',
    'admin.libraries.health.statusHealthy': 'Исправно',
    'admin.libraries.health.statusUnknown': 'Неизвестно',
    'admin.libraries.health.title': 'Состояние коннекторов и загрузки',
    'admin.libraries.settings.archivedLifecycle': 'В архиве',
    'admin.libraries.settings.chunkingTemplate': 'Шаблон чанкинга',
    'admin.libraries.settings.chunkingTemplates.code': 'Код',
    'admin.libraries.settings.chunkingTemplates.naive': 'Базовый',
    'admin.libraries.settings.chunkingTemplates.paper': 'Научная статья',
    'admin.libraries.settings.chunkingTemplates.qa': 'Вопросы и ответы',
    'admin.libraries.settings.chunkingTemplates.structured': 'Структурированный',
    'admin.libraries.settings.chunkingTemplates.tabular': 'Табличный',
    'admin.libraries.settings.lifecycle': 'Жизненный цикл',
    'admin.libraries.settings.rasterImageEngine': 'Движок растровых изображений',
    'admin.libraries.settings.recognitionEngines.docling': 'Docling',
    'admin.libraries.settings.recognitionEngines.vision': 'Модель компьютерного зрения',
    'admin.libraries.snapshot.connectorId': 'ID коннектора',
    'common.notAvailable': 'Нет данных',
  }

  return {
    inspectorQueries: {
      completedJobs: { data: { items: [] }, isError: false, isLoading: false },
      failedJobs: { data: { items: [] }, isError: false, isLoading: false },
      library: {
        data: {
          description: null,
          displayName: 'Библиотека',
          extractionPrompt: null,
          id: 'library-1',
          includeDocumentHintInMcpAnswers: true,
          ingestionReadiness: { missingBindingPurposes: [], ready: true },
          lifecycleState: 'active',
          recognitionPolicy: { rasterImageEngine: 'vision' },
          retrievalConfig: { lexical: { textSearchConfig: 'russian' } },
          runtimeReadiness: { missingBindingPurposes: [] },
          slug: 'library',
          webIngestPolicy: {
            crawlFilter: { allowPatterns: [], blockPatterns: [] },
            materializationFilter: { allowPatterns: [], blockPatterns: [] },
          },
          workspaceId: 'workspace-1',
        },
        isError: false,
        isLoading: false,
      },
      queue: { data: { summary: { total: 0 } }, isError: false, isLoading: false },
      retrieval: {
        data: { lexical: { textSearchConfig: 'russian' } },
        isError: false,
        isLoading: false,
      },
      state: {
        data: { state: { degradedState: 'healthy', failedDocumentCount: 0 } },
        isError: false,
        isLoading: false,
      },
    },
    locale: 'ru',
    t: vi.fn((key: string, options?: { defaultValue?: string }) => {
      return translations[key] ?? options?.defaultValue ?? key
    }),
  }
})

vi.mock('react-i18next', () => ({
  useTranslation: () => ({ i18n: { language: mocks.locale }, t: mocks.t }),
}))

vi.mock('@/features/admin/libraries/api/useLibrariesAdmin', () => ({
  useLibraryInspectorQueries: () => mocks.inspectorQueries,
}))

const row: AdminLibraryRow = {
  queuedJobCount: 0,
  costSummary: null,
  displayName: 'Библиотека',
  id: 'library-1',
  includeDocumentHintInMcpAnswers: true,
  lifecycleState: 'active',
  readiness: { missingBindingPurposes: [], ready: true },
  slug: 'library',
  workspace: { displayName: 'Пространство', id: 'workspace-1', slug: 'workspace' },
}

function renderInspector() {
  return render(
    <LibraryInspector
      row={row}
      open
      saving={false}
      deleting={false}
      onOpenChange={vi.fn()}
      onOpenDocuments={vi.fn(async () => undefined)}
      onConfigureAi={vi.fn()}
      onBackup={vi.fn()}
      onRestore={vi.fn()}
      onDelete={vi.fn(async () => undefined)}
      onSave={vi.fn(async () => undefined)}
    />,
  )
}

describe('LibraryInspector', () => {
  beforeEach(() => {
    mocks.inspectorQueries.state.data.state.degradedState = 'healthy'
    mocks.inspectorQueries.queue.data = { summary: { total: 0 } }
    mocks.t.mockClear()
  })

  it('keeps the pending-jobs counter readable when the queue payload has no summary', () => {
    // A partial server body must degrade to the not-available value instead of
    // taking the screen down: the summary used to be read straight off the
    // response.
    mocks.inspectorQueries.queue.data = {} as (typeof mocks.inspectorQueries.queue)['data']

    renderInspector()

    const pending = screen.getByText('Активные задачи').closest('div')
    expect(within(pending as HTMLElement).getByText('Нет данных')).toBeInTheDocument()
  })

  it('shows localized labels for settings enums instead of raw wire values', () => {
    renderInspector()

    const lifecycle = screen.getByRole('combobox', { name: 'Жизненный цикл' })
    const chunking = screen.getByRole('combobox', { name: /^Шаблон чанкинга/ })
    const recognition = screen.getByRole('combobox', {
      name: 'Движок растровых изображений',
    })

    expect(within(lifecycle).getByRole('option', { name: 'Активна' })).toBeInTheDocument()
    expect(within(chunking).getByRole('option', { name: 'Базовый' })).toBeInTheDocument()
    expect(
      within(recognition).getByRole('option', { name: 'Модель компьютерного зрения' }),
    ).toBeInTheDocument()
    expect(within(chunking).queryByRole('option', { name: 'naive' })).not.toBeInTheDocument()
    expect(within(recognition).queryByRole('option', { name: 'vision' })).not.toBeInTheDocument()
  })

  it('humanizes an unknown health state instead of hiding it behind a generic label', () => {
    mocks.inspectorQueries.state.data.state.degradedState = 'canonical_pipeline_failed'

    renderInspector()

    expect(screen.getByText('Canonical pipeline failed')).toBeInTheDocument()
    expect(screen.queryByText('Неизвестно')).not.toBeInTheDocument()
  })

  it('collapses inspector grids below the medium breakpoint', () => {
    renderInspector()

    expect(document.querySelector('.library-inspector-grid')).toHaveClass('max-md:grid-cols-1!')
    expect(document.querySelector('.connector-health dl')).toHaveClass('max-md:grid-cols-1!')
  })
})
