import { useQuery } from '@tanstack/react-query'
import { Braces, Clock3, FileJson, LoaderCircle, X } from 'lucide-react'
import { useMemo, useState } from 'react'
import { useTranslation } from 'react-i18next'

import { queries } from '@/shared/api'
import type { LlmContextDebugResponse } from '@/shared/api/query'
import { humanizeToken } from '@/shared/lib/enumLabels'
import { errorMessage } from '@/shared/lib/errorMessage'
import { CopyButton, IconButton, ScreenState, Segmented } from '@/shared/ui'
import type { EvidenceBundle } from '@/shared/types'

type DebugPanelProps = Readonly<{
  context: LlmContextDebugResponse | null
  contextError: string | null
  contextLoading: boolean
  evidence: EvidenceBundle | null
  executionId: string | null
  libraryId: string | undefined
  open: boolean
  onClose: () => void
}>

function formatDuration(durationMs: number | null | undefined): string {
  if (durationMs == null) return '—'
  if (durationMs < 1000) return `${Math.round(durationMs)} ms`
  return `${(durationMs / 1000).toFixed(1)} s`
}

export function DebugPanel({
  context,
  contextError,
  contextLoading,
  evidence,
  executionId,
  libraryId,
  open,
  onClose,
}: DebugPanelProps) {
  const { t } = useTranslation()
  const [tab, setTab] = useState('timeline')
  const executionQuery = useQuery({
    ...queries.getQueryExecutionOptions({ path: { executionId: executionId ?? '' } }),
    enabled: open && Boolean(executionId),
  })
  const systemPromptQuery = useQuery({
    ...queries.getAssistantSystemPromptOptions({
      query: libraryId ? { libraryId } : {},
    }),
    enabled: open && Boolean(libraryId),
  })
  const stages = useMemo(
    () =>
      executionQuery.data?.runtimeStageSummaries.map((stage) => ({
        stage: t(`assistant.runtimeStageLabels.${stage.stageKind}`, {
          defaultValue: stage.stageLabel || humanizeToken(stage.stageKind),
        }),
        time: formatDuration(stage.durationMs),
      })) ??
      evidence?.runtimeSummary?.stages.map((stage) => ({
        stage: t(`assistant.runtimeStageLabels.${stage.stage}`, {
          defaultValue: humanizeToken(stage.stage),
        }),
        time: formatDuration(stage.durationMs),
      })) ??
      [],
    [evidence?.runtimeSummary?.stages, executionQuery.data?.runtimeStageSummaries, t],
  )
  const loading =
    executionQuery.isLoading ||
    (tab === 'context' && (contextLoading || systemPromptQuery.isLoading)) ||
    (tab === 'raw' && (contextLoading || systemPromptQuery.isLoading))
  const visibleError = executionQuery.error
    ? errorMessage(executionQuery.error, t('assistant.llmContextUnavailable'))
    : null
  const promptError = systemPromptQuery.error
    ? errorMessage(systemPromptQuery.error, t('assistant.llmContextUnavailable'))
    : null
  const noExecutionSelected = !executionId && tab !== 'context'

  if (!open) return null

  return (
    <aside className="debug-panel">
      <header>
        <div>
          <strong>{t('assistant.debug')}</strong>
          {executionId ? (
            <span className="debug-execution-id">
              {t('assistant.execution')} · {executionId.slice(0, 8)}
              <CopyButton value={executionId} label={t('common.copy')} />
            </span>
          ) : (
            <span>{t('assistant.debugInspectorNoContext')}</span>
          )}
        </div>
        <IconButton label={t('common.close')} onClick={onClose}>
          <X size={16} />
        </IconButton>
      </header>
      <Segmented
        value={tab}
        onChange={setTab}
        label={t('assistant.debug')}
        items={[
          { value: 'timeline', label: t('assistant.timeline') },
          { value: 'context', label: t('assistant.context') },
          { value: 'raw', label: t('assistant.raw') },
        ]}
      />

      {loading && (
        <div className="state-card" aria-live="polite">
          <LoaderCircle className="spin" size={18} />
          <div>{t('assistant.debugInspectorLoading')}</div>
        </div>
      )}
      {!loading && visibleError && (
        <div className="state-card state-card--error" role="alert">
          <div>{visibleError}</div>
        </div>
      )}
      {!loading && !visibleError && !noExecutionSelected && tab === 'timeline' && (
        <ScreenState
          scenario={stages.length === 0 ? 'empty' : 'ready'}
          emptyText={t('assistant.mcpNoStageTelemetry')}
        >
          <div className="debug-timeline">
            {stages.map((stage, index) => {
              const Icon = index % 2 === 0 ? Clock3 : Braces
              return (
                <div key={`${stage.stage}-${index}`}>
                  <Icon size={14} />
                  <span>{stage.stage}</span>
                  <strong>{stage.time}</strong>
                </div>
              )
            })}
          </div>
        </ScreenState>
      )}
      {!loading && !visibleError && tab === 'context' && (
        <div className="debug-context">
          <section>
            <span>{t('assistant.systemPrompt')}</span>
            <pre>
              {promptError ??
                systemPromptQuery.data?.rendered ??
                systemPromptQuery.data?.template ??
                '—'}
            </pre>
          </section>
          {contextError && (
            <section>
              <span>{t('assistant.context')}</span>
              <pre>{contextError}</pre>
            </section>
          )}
          {context?.question && (
            <section>
              <span>{t('assistant.debugQuestion')}</span>
              <pre>{context.question}</pre>
            </section>
          )}
          {context?.iterations.map((iteration) => (
            <section key={iteration.iteration}>
              <span>
                {t('assistant.iteration')} {iteration.iteration} · {iteration.modelName}
              </span>
              <pre>{JSON.stringify(iteration.requestMessages, null, 2)}</pre>
            </section>
          ))}
        </div>
      )}
      {!loading && !visibleError && !noExecutionSelected && tab === 'raw' && (
        <div className="debug-context">
          <section>
            <span>
              <FileJson size={12} /> {t('assistant.raw')}
            </span>
            <pre className="raw-json">
              {JSON.stringify(
                {
                  execution: executionQuery.data ?? null,
                  llmContext: context,
                  systemPrompt: systemPromptQuery.data ?? null,
                },
                null,
                2,
              )}
            </pre>
          </section>
        </div>
      )}
      {!loading && !visibleError && noExecutionSelected && (
        <ScreenState scenario="empty" emptyText={t('assistant.debugInspectorEmpty')}>
          {null}
        </ScreenState>
      )}
    </aside>
  )
}
