import { Archive, Download, ExternalLink, LoaderCircle, Settings2, Upload } from 'lucide-react'
import { useState, type FormEvent } from 'react'
import { useTranslation } from 'react-i18next'
import { toast } from 'sonner'
import type {
  CatalogLibraryResponse,
  IngestJobListPageResponse,
  IngestJobResponse,
  RetrievalConfig,
} from '@/shared/api/generated'
import { errorMessage } from '@/shared/lib/errorMessage'
import { humanizeToken, libraryLifecycleStateLabel } from '@/shared/lib/enumLabels'
import { Badge, Button, Modal } from '@/shared/ui'
import { useLibraryInspectorQueries } from '@/features/admin/libraries/api/useLibrariesAdmin'
import {
  adaptLibrarySettings,
  CHUNKING_TEMPLATES,
  type AdminLibraryRow,
  type LibrarySettingsDraft,
} from '@/features/admin/libraries/model/librariesAdapter'

type LibraryInspectorProps = {
  row: AdminLibraryRow
  open: boolean
  saving: boolean
  deleting: boolean
  onOpenChange: (open: boolean) => void
  onOpenDocuments: (row: AdminLibraryRow) => Promise<void>
  onConfigureAi: (row: AdminLibraryRow) => void
  onBackup: (row: AdminLibraryRow) => void
  onRestore: (row: AdminLibraryRow) => void
  onDelete: (row: AdminLibraryRow) => Promise<void>
  onSave: (
    libraryId: string,
    draft: LibrarySettingsDraft,
    current: CatalogLibraryResponse,
  ) => Promise<void>
}

function latestConnectorJob(page: IngestJobListPageResponse | undefined): IngestJobResponse | null {
  return page?.items.find((item) => item.job.connector_id != null) ?? null
}

function connectorTimestamp(job: IngestJobResponse | null): string | null {
  if (!job) return null
  return job.latestAttempt?.finished_at ?? job.job.completed_at ?? job.job.queued_at
}

function healthTone(state: string | undefined): string {
  if (state === 'healthy') return 'ready'
  if (state === 'degraded') return 'failed'
  if (state === 'processing') return 'processing'
  return 'warning'
}

function ConnectorHealth({
  queries,
  locale,
}: Readonly<{
  queries: ReturnType<typeof useLibraryInspectorQueries>
  locale: string
}>) {
  const { t } = useTranslation()
  const latestSuccess = latestConnectorJob(queries.completedJobs.data)
  const latestFailure = latestConnectorJob(queries.failedJobs.data)
  const timestamp = connectorTimestamp(latestSuccess)
  const lastSync = timestamp ? new Date(timestamp).toLocaleString(locale) : null
  const connectorId = latestSuccess?.job.connector_id ?? latestFailure?.job.connector_id ?? null
  const degradedState = queries.state.data?.state?.degradedState
  const stateLabel = degradedState
    ? t(
        `admin.libraries.health.status${degradedState.charAt(0).toUpperCase()}${degradedState.slice(1)}`,
        { defaultValue: humanizeToken(degradedState) },
      )
    : t('admin.libraries.health.statusUnknown')
  const healthValue = (loading: boolean, error: boolean, value: string | number | null) => {
    if (loading) return t('common.loading')
    if (error || value == null) return t('common.notAvailable')
    return value
  }

  return (
    <div className="connector-health">
      <div>
        <Badge tone={healthTone(degradedState)}>{stateLabel}</Badge>
        <strong>{t('admin.libraries.health.title')}</strong>
      </div>
      <dl className="max-md:grid-cols-1!">
        <div>
          <dt>{t('admin.libraries.health.pendingJobs')}</dt>
          <dd>
            {healthValue(
              queries.queue.isLoading,
              queries.queue.isError,
              queries.queue.data?.summary?.total ?? null,
            )}
          </dd>
        </div>
        <div>
          <dt>{t('admin.libraries.health.failedDocuments')}</dt>
          <dd>
            {healthValue(
              queries.state.isLoading,
              queries.state.isError,
              queries.state.data?.state?.failedDocumentCount ?? null,
            )}
          </dd>
        </div>
        <div>
          <dt>{t('admin.libraries.health.lastSuccessfulSync')}</dt>
          <dd>
            {healthValue(queries.completedJobs.isLoading, queries.completedJobs.isError, lastSync)}
          </dd>
        </div>
        <div>
          <dt>{t('admin.libraries.snapshot.connectorId')}</dt>
          <dd>
            {healthValue(
              queries.completedJobs.isLoading || queries.failedJobs.isLoading,
              queries.completedJobs.isError && queries.failedJobs.isError,
              connectorId,
            )}
          </dd>
        </div>
      </dl>
    </div>
  )
}

function SettingsForm({
  formId,
  library,
  retrieval,
  onSave,
}: Readonly<{
  formId: string
  library: CatalogLibraryResponse
  retrieval: RetrievalConfig
  onSave: (draft: LibrarySettingsDraft) => Promise<void>
}>) {
  const { t } = useTranslation()
  const [draft, setDraft] = useState<LibrarySettingsDraft>(() =>
    adaptLibrarySettings(library, retrieval),
  )

  const patchDraft = <K extends keyof LibrarySettingsDraft>(
    key: K,
    value: LibrarySettingsDraft[K],
  ) => setDraft((current) => ({ ...current, [key]: value }))

  const submit = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault()
    if (!draft.displayName.trim() || !draft.slug.trim() || !draft.textSearchConfig.trim()) {
      toast.error(t('admin.libraries.settings.requiredFields'))
      return
    }
    await onSave(draft)
  }

  return (
    <form id={formId} className="library-settings-form" onSubmit={submit}>
      <div className="form-section">
        <h3>{t('admin.libraries.settings.catalogTitle')}</h3>
        <div className="field-grid">
          <label className="field">
            <span>{t('admin.libraries.settings.displayName')}</span>
            <input
              className="input"
              value={draft.displayName}
              onChange={(event) => patchDraft('displayName', event.target.value)}
            />
          </label>
          <label className="field">
            <span>{t('admin.libraries.settings.slug')}</span>
            <input
              className="input"
              value={draft.slug}
              onChange={(event) => patchDraft('slug', event.target.value)}
            />
          </label>
          <label className="field field--wide">
            <span>{t('admin.libraries.settings.description')}</span>
            <textarea
              className="input"
              value={draft.description}
              onChange={(event) => patchDraft('description', event.target.value)}
            />
          </label>
          <label className="field">
            <span>{t('admin.libraries.settings.lifecycle')}</span>
            <select
              className="input"
              value={draft.lifecycleState}
              onChange={(event) =>
                patchDraft('lifecycleState', event.target.value as 'active' | 'archived')
              }
            >
              <option value="active">{libraryLifecycleStateLabel('active', t)}</option>
              <option value="archived">{libraryLifecycleStateLabel('archived', t)}</option>
            </select>
          </label>
          <label className="field">
            <span>{t('admin.libraries.settings.chunkingTemplate')}</span>
            <select className="input" value={draft.chunkingTemplate} disabled>
              {CHUNKING_TEMPLATES.map((template) => (
                <option key={template} value={template}>
                  {t(`admin.libraries.settings.chunkingTemplates.${template}`, {
                    defaultValue: humanizeToken(template),
                  })}
                </option>
              ))}
            </select>
            <small className="field-hint">{t('admin.libraries.settings.chunkingReadonly')}</small>
          </label>
          <label className="field field--wide">
            <span>{t('admin.libraries.settings.extractionPrompt')}</span>
            <textarea
              className="input"
              value={draft.extractionPrompt}
              onChange={(event) => patchDraft('extractionPrompt', event.target.value)}
            />
          </label>
          <label className="settings-checkbox field--wide">
            <input
              type="checkbox"
              aria-label={t('admin.mcp.includeDocumentHint')}
              checked={draft.includeDocumentHintInMcpAnswers}
              onChange={(event) =>
                patchDraft('includeDocumentHintInMcpAnswers', event.target.checked)
              }
            />
            <span>
              <strong>{t('admin.mcp.includeDocumentHint')}</strong>
              <small>{t('admin.mcp.includeDocumentHintHelp')}</small>
            </span>
          </label>
        </div>
      </div>
      <div className="form-section">
        <h3>{t('admin.libraries.settings.processingTitle')}</h3>
        <div className="field-grid">
          <label className="field">
            <span>{t('admin.libraries.settings.rasterImageEngine')}</span>
            <select
              className="input"
              value={draft.recognitionEngine}
              onChange={(event) =>
                patchDraft('recognitionEngine', event.target.value as 'docling' | 'vision')
              }
            >
              <option value="docling">
                {t('admin.libraries.settings.recognitionEngines.docling')}
              </option>
              <option value="vision">
                {t('admin.libraries.settings.recognitionEngines.vision')}
              </option>
            </select>
          </label>
          <label className="field">
            <span>{t('admin.libraries.settings.textSearchConfig')}</span>
            <input
              className="input code-input"
              value={draft.textSearchConfig}
              onChange={(event) => patchDraft('textSearchConfig', event.target.value)}
            />
          </label>
        </div>
      </div>
      <div className="form-section">
        <h3>{t('admin.libraries.settings.webIngestTitle')}</h3>
        <p className="library-settings-hint">{t('admin.libraries.settings.patternsHint')}</p>
        <div className="field-grid">
          <label className="field">
            <span>{t('admin.libraries.settings.crawlAllowPatterns')}</span>
            <textarea
              className="input code-input"
              value={draft.crawlAllowPatterns}
              onChange={(event) => patchDraft('crawlAllowPatterns', event.target.value)}
            />
          </label>
          <label className="field">
            <span>{t('admin.libraries.settings.crawlBlockPatterns')}</span>
            <textarea
              className="input code-input"
              value={draft.crawlBlockPatterns}
              onChange={(event) => patchDraft('crawlBlockPatterns', event.target.value)}
            />
          </label>
          <label className="field">
            <span>{t('admin.libraries.settings.materializationAllowPatterns')}</span>
            <textarea
              className="input code-input"
              value={draft.materializationAllowPatterns}
              onChange={(event) => patchDraft('materializationAllowPatterns', event.target.value)}
            />
          </label>
          <label className="field">
            <span>{t('admin.libraries.settings.materializationBlockPatterns')}</span>
            <textarea
              className="input code-input"
              value={draft.materializationBlockPatterns}
              onChange={(event) => patchDraft('materializationBlockPatterns', event.target.value)}
            />
          </label>
        </div>
      </div>
    </form>
  )
}

export function LibraryInspector({
  row,
  open,
  saving,
  deleting,
  onOpenChange,
  onOpenDocuments,
  onConfigureAi,
  onBackup,
  onRestore,
  onDelete,
  onSave,
}: Readonly<LibraryInspectorProps>) {
  const { t, i18n } = useTranslation()
  const queries = useLibraryInspectorQueries(row.id, open)
  const formId = `library-settings-${row.id}`
  const loading = queries.library.isLoading || queries.retrieval.isLoading
  const failed = queries.library.isError || queries.retrieval.isError

  const save = async (draft: LibrarySettingsDraft) => {
    if (!queries.library.data) return
    try {
      await onSave(row.id, draft, queries.library.data)
      toast.success(t('admin.libraries.settings.saveSuccess'))
    } catch (error: unknown) {
      toast.error(errorMessage(error, t('admin.libraries.settings.saveFailed')))
    }
  }

  const remove = async () => {
    if (!window.confirm(t('admin.libraries.settings.deleteConfirm', { name: row.displayName }))) {
      return
    }
    await onDelete(row)
  }

  return (
    <Modal
      open={open}
      onOpenChange={onOpenChange}
      title={row.displayName}
      description={`${row.workspace.displayName} · ${row.slug}`}
      wide
      footer={
        <>
          <Button variant="danger" disabled={deleting || saving} onClick={remove}>
            {deleting && <LoaderCircle className="spin" size={14} />}
            {t('common.delete')}
          </Button>
          <Button disabled={deleting || saving} onClick={() => onOpenChange(false)}>
            {t('common.close')}
          </Button>
          <Button
            variant="primary"
            type="submit"
            form={formId}
            disabled={loading || failed || deleting || saving}
          >
            {saving && <LoaderCircle className="spin" size={14} />}
            {t('common.save')}
          </Button>
        </>
      }
    >
      <div className="library-inspector-grid max-md:grid-cols-1!">
        <ConnectorHealth queries={queries} locale={i18n.language} />
        <div className="snapshot-actions">
          <Button onClick={() => onOpenDocuments(row)}>
            <ExternalLink size={14} />
            {t('admin.libraries.openDocuments')}
          </Button>
          <Button onClick={() => onConfigureAi(row)}>
            <Settings2 size={14} />
            {t('admin.libraries.configureAi')}
          </Button>
          <Button onClick={() => onBackup(row)}>
            <Download size={14} />
            {t('admin.snapshot.export')}
          </Button>
          <Button onClick={() => onRestore(row)}>
            <Upload size={14} />
            {t('admin.snapshot.import')}
          </Button>
          <Button onClick={() => onBackup(row)}>
            <Archive size={14} />
            {t('admin.libraries.snapshot.exportWorkspace')}
          </Button>
        </div>
      </div>
      {loading && (
        <div className="library-inspector-state">
          <LoaderCircle className="spin" size={18} />
          {t('common.loading')}
        </div>
      )}
      {failed && (
        <div className="library-inspector-state library-inspector-state--error" role="alert">
          {t('admin.libraries.settings.loadFailed')}
        </div>
      )}
      {queries.library.data && queries.retrieval.data && (
        <SettingsForm
          key={row.id}
          formId={formId}
          library={queries.library.data}
          retrieval={queries.retrieval.data}
          onSave={save}
        />
      )}
    </Modal>
  )
}
