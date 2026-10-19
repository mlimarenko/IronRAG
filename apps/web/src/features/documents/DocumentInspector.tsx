import {
  Download,
  FilePenLine,
  MoreHorizontal,
  Network,
  Pencil,
  RefreshCw,
  Trash2,
  Upload,
  X,
} from 'lucide-react'
import { useCallback, useRef, useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { useTranslation } from 'react-i18next'
import { documentStatusLabel } from '@/shared/lib/enumLabels'

import type { DocumentLifecycleDetail } from '@/shared/api'
import { formatBytes, formatDate, formatDocumentSize } from '@/shared/format'
import { Badge, Button, CopyButton, IconButton, Modal, Progress, Segmented } from '@/shared/ui'
import type { DocumentItem, Locale } from '@/shared/types'
import { buildDocumentFailureNotice, humanizeDocumentStage } from '@/shared/lib/document-processing'

import { DocumentEditor } from './DocumentEditor'
import { isEditorEditableSourceFormat } from './components/editor/editorSurfaceMode'
import { useDocumentEditor } from './components/editor/useDocumentEditor'
import {
  FactsSection,
  PipelineSection,
  RevisionsSection,
  SegmentsSection,
} from './components/documents-page/DocumentInspectorSections'
import { useDocumentInspectorController } from './components/documents-page/useDocumentInspectorController'
import { canReprocessDocument, getDocumentSourceFormat } from './model/documentAdapter'
import { DOCUMENT_FILE_INPUT_ACCEPT } from './model/uploadAccept'

type Section = 'pipeline' | 'revisions' | 'segments' | 'facts'
type Props = {
  activateListPollGrace: () => void
  canDelete: boolean
  canEdit: boolean
  document: DocumentItem | null
  errorMessage: (error: unknown, fallback: string) => string
  fetchSelectedDetail: (documentId: string) => Promise<void>
  lifecycle: DocumentLifecycleDetail | null
  loadFirstPage: () => Promise<void>
  locale: Locale
  onClose: () => void
  onDocumentHintUpdated: (documentId: string, hint: string | null) => void
  onSelectDocument: (document: DocumentItem) => void
}

export function DocumentInspector(props: Readonly<Props>) {
  if (!props.document) return null
  return <DocumentInspectorContent key={props.document.id} {...props} document={props.document} />
}

function DocumentInspectorContent(props: Props & { document: DocumentItem }) {
  const { t } = useTranslation()
  const navigate = useNavigate()
  const [section, setSection] = useState<Section>('pipeline')
  const [hintEditing, setHintEditing] = useState(false)
  const [hint, setHint] = useState(props.document.documentHint ?? '')
  const [deleteOpen, setDeleteOpen] = useState(false)
  const [replaceOpen, setReplaceOpen] = useState(false)
  const [replaceFile, setReplaceFile] = useState<File | null>(null)
  const replaceInput = useRef<HTMLInputElement>(null)
  const inspector = useDocumentInspectorController({
    activateListPollGrace: props.activateListPollGrace,
    document: props.document,
    errorMessage: props.errorMessage,
    fetchSelectedDetail: props.fetchSelectedDetail,
    loadFirstPage: props.loadFirstPage,
    onClose: props.onClose,
    onDocumentHintUpdated: props.onDocumentHintUpdated,
    section,
    t,
  })
  const editorAvailability = useCallback(
    (document: DocumentItem | null) => {
      if (document?.status !== 'ready')
        return {
          enabled: false,
          readOnly: false,
          reason:
            document?.status === 'failed'
              ? t('documents.editUnavailableFailed')
              : t('documents.editUnavailableProcessing'),
        }
      return {
        enabled: true,
        readOnly: !isEditorEditableSourceFormat(getDocumentSourceFormat(document)),
        reason: null,
      }
    },
    [t],
  )
  const editor = useDocumentEditor({
    editorAvailability,
    errorMessage: props.errorMessage,
    onDocumentSaved: async (documentId) => {
      await props.loadFirstPage()
      await props.fetchSelectedDetail(documentId)
    },
    onDocumentSelected: props.onSelectDocument,
    selectedDocumentId: props.document?.id ?? null,
    t,
  })
  const document = props.document
  const failure =
    document.status === 'failed'
      ? (document.failureNotice ??
        buildDocumentFailureNotice(
          {
            failureCode: document.failureCode,
            failureMessage: document.failureMessage,
            stage: document.stage,
          },
          t,
        ))
      : null
  const editorState = editorAvailability(document)
  const graphReady = ['readable', 'graph_sparse', 'graph_ready'].includes(document.readiness)
  // `ready` is reprocessed to rebuild a missing index, not to retry a failed
  // attempt, so the control is named for what it does and is not gated by the
  // server's failed-attempt `canRetry` flag.
  const reprocessAvailable = canReprocessDocument(document)
  const reprocessLabel =
    document.status === 'ready' ? t('documents.reprocess') : t('documents.retryProcessing')
  const stage = humanizeDocumentStage(document.stage, t)
  const progressLabel = stage ?? t('documents.pipeline')

  return (
    <>
      <button
        type="button"
        className="document-inspector-backdrop"
        aria-label={t('common.close')}
        onClick={props.onClose}
      />
      <aside
        className="document-inspector"
        role="dialog"
        aria-modal="false"
        aria-label={t('documents.inspector')}
      >
        <div className="dialog-heading">
          <strong>{t('documents.inspector')}</strong>
          <IconButton label={t('common.close')} onClick={props.onClose}>
            <X size={18} />
          </IconButton>
        </div>
        <div className="inspector">
          <div className="inspector-title">
            <div>
              <h2>{document.fileName ?? t('common.notAvailable')}</h2>
            </div>
            <Badge tone={document.status}>{documentStatusLabel(document.status, t)}</Badge>
          </div>
          {document.status === 'processing' && (
            <div className="inspector-progress">
              <span>
                {stage && `${stage} · `}
                {document.progressPercent ?? 0}%
              </span>
              <Progress value={document.progressPercent ?? 0} label={progressLabel} />
            </div>
          )}
          {failure && (
            <div className="failure-card">
              <strong>{failure.title}</strong>
              <p>{failure.summary}</p>
              <dl>
                <div>
                  <dt>{t('documents.impact')}</dt>
                  <dd>{failure.impact}</dd>
                </div>
                <div>
                  <dt>{t('documents.failureActionLabel')}</dt>
                  <dd>{failure.action}</dd>
                </div>
              </dl>
              {failure.diagnosticCode && (
                <CopyButton value={failure.diagnosticCode} label={failure.diagnosticCode} />
              )}
            </div>
          )}
          <div className="inspector-actions">
            {props.canEdit && (
              <Button
                size="sm"
                disabled={!editorState.enabled}
                title={editorState.reason ?? undefined}
                onClick={() => void editor.openEditor(document)}
              >
                <FilePenLine size={14} />
                {editorState.readOnly ? t('documents.viewDocument') : t('common.edit')}
              </Button>
            )}
            <Button size="sm" onClick={() => void inspector.download()}>
              <Download size={14} />
              {t('documents.downloadDocument')}
            </Button>
            {(graphReady || props.canEdit || props.canDelete) && (
              <details className="inspector-more">
                <summary
                  className="button button--secondary button--sm"
                  aria-label={t('documents.moreActions')}
                  title={t('documents.moreActions')}
                >
                  <MoreHorizontal size={16} />
                  <span className="sr-only">{t('documents.moreActions')}</span>
                </summary>
                <div>
                  {graphReady && (
                    <Button
                      size="sm"
                      variant="ghost"
                      onClick={() => navigate(`/graph?nodeId=${encodeURIComponent(document.id)}`)}
                    >
                      <Network size={14} />
                      {t('documents.viewInGraph')}
                    </Button>
                  )}
                  {props.canEdit && (
                    <>
                      <Button
                        size="sm"
                        variant="ghost"
                        disabled={!reprocessAvailable}
                        title={reprocessAvailable ? undefined : t('documents.retryUnavailable')}
                        onClick={() => void inspector.retry()}
                      >
                        <RefreshCw size={14} />
                        {reprocessLabel}
                      </Button>
                      <Button size="sm" variant="ghost" onClick={() => setReplaceOpen(true)}>
                        <Upload size={14} />
                        {t('documents.replaceFile')}
                      </Button>
                    </>
                  )}
                  {props.canDelete && (
                    <Button size="sm" variant="danger" onClick={() => setDeleteOpen(true)}>
                      <Trash2 size={14} />
                      {t('documents.delete')}
                    </Button>
                  )}
                </div>
              </details>
            )}
          </div>
          <div className="inspector-facts">
            <div>
              <span>{t('documents.type')}</span>
              <strong>{document.fileType ?? '—'}</strong>
            </div>
            <div>
              <span>{t('documents.size')}</span>
              <strong>{formatDocumentSize(document.fileSize)}</strong>
            </div>
            <div>
              <span>{t('documents.uploaded')}</span>
              <strong>
                {formatDate(document.uploadedAt, props.locale, { dateStyle: 'medium' })}
              </strong>
            </div>
          </div>
          <details className="inspector-technical">
            <summary>{t('documents.technicalData')}</summary>
            <div>
              <span>{t('documents.externalKey')}</span>
              <div className="copy-row">
                <code>{document.externalKey ?? '—'}</code>
                {document.externalKey && <CopyButton value={document.externalKey} />}
              </div>
              <span>{t('documents.documentId')}</span>
              <div className="copy-row">
                <code>{document.id}</code>
                <CopyButton value={document.id} />
              </div>
            </div>
          </details>
          <div className="inspector-hint">
            <div className="inspector-hint__label">
              <span>{t('documents.documentHint')}</span>
              {props.canEdit && !hintEditing && (
                <IconButton
                  label={t('documents.documentHintEditAria')}
                  onClick={() => setHintEditing(true)}
                >
                  <Pencil size={13} />
                </IconButton>
              )}
            </div>
            {hintEditing ? (
              <div className="hint-editor">
                <textarea
                  className="input"
                  value={hint}
                  onChange={(event) => setHint(event.target.value)}
                  maxLength={1024}
                  autoFocus
                  placeholder={t('documents.documentHintEditPlaceholder')}
                />
                <small>{t('documents.documentHintHelp')}</small>
                <Button
                  size="sm"
                  onClick={() =>
                    void inspector.saveHint(hint).then((saved) => saved && setHintEditing(false))
                  }
                >
                  {t('common.save')}
                </Button>
                <Button
                  size="sm"
                  variant="ghost"
                  onClick={() => {
                    setHint(document.documentHint ?? '')
                    setHintEditing(false)
                  }}
                >
                  {t('common.cancel')}
                </Button>
              </div>
            ) : (
              <p>{document.documentHint || '—'}</p>
            )}
          </div>
          <Segmented
            value={section}
            onChange={(value) => setSection(value as Section)}
            label={t('common.details')}
            items={[
              { value: 'pipeline', label: t('documents.pipeline') },
              { value: 'revisions', label: t('documents.revisions') },
              { value: 'segments', label: t('documents.segments') },
              { value: 'facts', label: t('documents.facts') },
            ]}
          />
          {section === 'pipeline' && (
            <PipelineSection lifecycle={props.lifecycle} locale={props.locale} />
          )}
          {section === 'revisions' && (
            <RevisionsSection controller={inspector} locale={props.locale} />
          )}
          {section === 'segments' && <SegmentsSection controller={inspector} />}
          {section === 'facts' && <FactsSection controller={inspector} />}
        </div>
      </aside>
      <Modal
        open={deleteOpen}
        onOpenChange={setDeleteOpen}
        title={t('documents.deleteDoc')}
        description={t('documents.confirmDelete', {
          name: document.fileName ?? t('common.notAvailable'),
        })}
        footer={
          <>
            <Button onClick={() => setDeleteOpen(false)}>{t('common.cancel')}</Button>
            <Button variant="danger" onClick={() => void inspector.remove()}>
              {t('documents.delete')}
            </Button>
          </>
        }
      >
        <p>
          {t('documents.confirmDelete', { name: document.fileName ?? t('common.notAvailable') })}
        </p>
      </Modal>
      <Modal
        open={replaceOpen}
        onOpenChange={(open) => {
          setReplaceOpen(open)
          if (!open) setReplaceFile(null)
        }}
        title={t('documents.replaceFileTitle')}
        description={t('documents.replaceFileDesc', {
          name: document.fileName ?? t('common.notAvailable'),
        })}
        footer={
          <>
            <Button onClick={() => setReplaceOpen(false)}>{t('common.cancel')}</Button>
            <Button
              variant="primary"
              disabled={!replaceFile}
              onClick={() => {
                if (replaceFile)
                  void inspector
                    .replace(replaceFile)
                    .then((saved) => saved && setReplaceOpen(false))
              }}
            >
              {t('documents.replace')}
            </Button>
          </>
        }
      >
        <button
          type="button"
          className="replace-dropzone"
          onClick={() => replaceInput.current?.click()}
        >
          <Upload size={24} />
          <strong>{replaceFile?.name ?? t('documents.selectFile')}</strong>
          <span>{replaceFile ? formatBytes(replaceFile.size) : t('documents.selectFileHint')}</span>
        </button>
        <input
          ref={replaceInput}
          className="visually-hidden"
          type="file"
          accept={DOCUMENT_FILE_INPUT_ACCEPT}
          onChange={(event) => setReplaceFile(event.target.files?.[0] ?? null)}
        />
      </Modal>
      {editor.editorDocument && (
        <DocumentEditor
          document={editor.editorDocument}
          error={editor.editorError}
          loading={editor.editorLoading}
          markdown={editor.editorMarkdown}
          onOpenChange={editor.handleEditorOpenChange}
          onSave={editor.saveEditor}
          open={editor.editorOpen}
          readOnly={editor.editorReadOnly}
          saving={editor.editorSaving}
        />
      )}
    </>
  )
}
