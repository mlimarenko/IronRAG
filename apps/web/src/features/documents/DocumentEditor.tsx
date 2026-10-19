import { Bold, Code2, Italic, List, Save, WrapText } from 'lucide-react'
import { useMemo, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'

import type { DocumentItem } from '@/shared/types'
import { Badge, Button, Modal } from '@/shared/ui'
import { createEditorBaseline, isEditorContentDirty } from './components/editor/editorBaseline'
import {
  isPlainTextSourceFormat,
  resolveEditorSurfaceMode,
} from './components/editor/editorSurfaceMode'
import type { EditorSurfaceMode } from './components/editor/editorSurfaceMode'
import { formatDocumentTypeLabel, getDocumentSourceFormat } from './model/documentAdapter'

// Spelled out rather than assembled as `documents.editor.${surface}Mode`: a
// key built from a template makes every sibling in the namespace look
// reachable to the i18n audit. Plain text reads as prose to the author, so it
// shares the prose label.
const EDITOR_SURFACE_LABEL_KEYS: Readonly<Record<EditorSurfaceMode, string>> = {
  code: 'documents.editor.codeMode',
  prose: 'documents.editor.proseMode',
  raw_text: 'documents.editor.proseMode',
  table: 'documents.editor.tableMode',
}

type Props = {
  document: DocumentItem
  error: string | null
  loading: boolean
  markdown: string
  onOpenChange: (open: boolean) => void
  onSave: (markdown: string) => Promise<void>
  open: boolean
  readOnly: boolean
  saving: boolean
}

export function DocumentEditor(props: Readonly<Props>) {
  const { t } = useTranslation()
  if (props.loading)
    return (
      <Modal
        open={props.open}
        onOpenChange={props.onOpenChange}
        wide
        title={props.readOnly ? t('documents.editor.viewerTitle') : t('documents.editor.title')}
        description={props.document.fileName ?? undefined}
      >
        <div className="state-card">
          <strong>{t('documents.editor.loading')}</strong>
        </div>
      </Modal>
    )
  return <EditorSession key={props.document.id} {...props} />
}

// Read-only outranks the dirty flag: a viewer cannot have unsaved work.
function editorStatus(readOnly: boolean, dirty: boolean): 'readOnly' | 'unsaved' | 'synced' {
  if (readOnly) return 'readOnly'
  return dirty ? 'unsaved' : 'synced'
}

function EditorSession({
  document,
  error,
  markdown,
  onOpenChange,
  onSave,
  open,
  readOnly,
  saving,
}: Readonly<Props>) {
  const { t } = useTranslation()
  const [draft, setDraft] = useState(markdown)
  const [baseline] = useState(() => createEditorBaseline(markdown))
  const [wrap, setWrap] = useState(true)
  const textareaRef = useRef<HTMLTextAreaElement>(null)
  const dirty = isEditorContentDirty(baseline, draft)
  const sourceFormat = getDocumentSourceFormat(document)
  const documentTypeLabel = formatDocumentTypeLabel(document.fileType, document.sourceKind, t, {
    sourceUri: document.sourceUri,
    fileName: document.fileName ?? undefined,
  })
  const surface = useMemo(
    () =>
      isPlainTextSourceFormat(sourceFormat)
        ? 'raw_text'
        : resolveEditorSurfaceMode({ markdown: draft, sourceFormat }),
    [sourceFormat, draft],
  )
  const requestClose = (nextOpen: boolean) => {
    if (!nextOpen && dirty && !window.confirm(t('documents.editor.unsavedConfirm'))) return
    onOpenChange(nextOpen)
  }
  const surround = (prefix: string, suffix = prefix) => {
    const textarea = textareaRef.current
    if (!textarea) return
    const start = textarea.selectionStart
    const end = textarea.selectionEnd
    setDraft(
      `${draft.slice(0, start)}${prefix}${draft.slice(start, end)}${suffix}${draft.slice(end)}`,
    )
    requestAnimationFrame(() => textarea.focus())
  }
  const status = {
    readOnly: t('documents.editor.readOnly'),
    unsaved: t('documents.editor.unsaved'),
    synced: t('documents.editor.synced'),
  }[editorStatus(readOnly, dirty)]
  return (
    <Modal
      open={open}
      onOpenChange={requestClose}
      wide
      title={readOnly ? t('documents.editor.viewerTitle') : t('documents.editor.title')}
      description={document.fileName ?? undefined}
      footer={
        <>
          <span className={`save-state ${dirty ? '' : 'is-saved'}`}>{status}</span>
          <Button onClick={() => requestClose(false)}>{t('common.close')}</Button>
          {!readOnly && (
            <Button
              variant="primary"
              disabled={!dirty || saving}
              onClick={() => void onSave(draft)}
            >
              <Save size={15} />
              {saving ? t('documents.editor.saving') : t('documents.editor.save')}
            </Button>
          )}
        </>
      }
    >
      <div className={`editor-shell editor-shell--${surface}`}>
        <div className="editor-meta">
          <Badge>{t(EDITOR_SURFACE_LABEL_KEYS[surface])}</Badge>
          <span>{documentTypeLabel}</span>
        </div>
        {!readOnly && surface === 'prose' && (
          <div className="editor-toolbar">
            <Button
              size="icon"
              variant="ghost"
              aria-label={t('documents.editor.bold')}
              onClick={() => surround('**')}
            >
              <Bold size={15} />
            </Button>
            <Button
              size="icon"
              variant="ghost"
              aria-label={t('documents.editor.italic')}
              onClick={() => surround('_')}
            >
              <Italic size={15} />
            </Button>
            <Button
              size="icon"
              variant="ghost"
              aria-label={t('documents.editor.bullets')}
              onClick={() => surround('- ', '')}
            >
              <List size={15} />
            </Button>
            <Button
              size="icon"
              variant="ghost"
              aria-label={t('documents.editor.code')}
              onClick={() => surround('`', '`')}
            >
              <Code2 size={15} />
            </Button>
            <Button
              size="icon"
              variant="ghost"
              aria-label={t('documents.editor.lineWrap')}
              aria-pressed={wrap}
              onClick={() => setWrap((value) => !value)}
            >
              <WrapText size={15} />
            </Button>
          </div>
        )}
        <textarea
          ref={textareaRef}
          className={`document-editor-surface document-editor-surface--${surface} ${wrap ? 'is-wrapped' : ''}`}
          value={draft}
          onChange={(event) => setDraft(event.target.value)}
          readOnly={readOnly}
          disabled={saving}
          spellCheck={surface === 'prose'}
          aria-label={document.fileName ?? t('common.notAvailable')}
        />
        {error && (
          <div className="inline-alert" role="alert">
            {error}
          </div>
        )}
      </div>
    </Modal>
  )
}
