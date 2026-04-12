import { useEffect, useMemo, useState } from 'react';
import type { TFunction } from 'i18next';
import { Loader2 } from 'lucide-react';
import { useEditor, type Editor } from '@tiptap/react';
import StarterKit from '@tiptap/starter-kit';
import { Table } from '@tiptap/extension-table';
import TableCell from '@tiptap/extension-table-cell';
import TableHeader from '@tiptap/extension-table-header';
import TableRow from '@tiptap/extension-table-row';
import { Markdown } from '@tiptap/markdown';

import { Button } from '@/components/ui/button';

import { createEditorBaseline, isEditorContentDirty, type DirtyStateBaseline } from './editorBaseline';
import { DocumentEditorCanvas } from './DocumentEditorCanvas';
import { DocumentEditorOverlay } from './DocumentEditorOverlay';
import { DocumentEditorToolbar } from './DocumentEditorToolbar';
import { resolveEditorSurfaceMode } from './editorSurfaceMode';

type DocumentEditorShellProps = {
  documentName: string;
  error: string | null;
  loading: boolean;
  markdown: string;
  onOpenChange: (open: boolean) => void;
  onSave: (markdown: string) => void | Promise<void>;
  open: boolean;
  saving: boolean;
  sourceFormat?: string;
  t: TFunction;
};

const editorExtensions = [
  StarterKit,
  Table.configure({ resizable: true }),
  TableRow,
  TableHeader,
  TableCell,
  Markdown,
];

export function DocumentEditorShell({
  documentName,
  error,
  loading,
  markdown,
  onOpenChange,
  onSave,
  open,
  saving,
  sourceFormat,
  t,
}: DocumentEditorShellProps) {
  const surfaceMode = useMemo(
    () => resolveEditorSurfaceMode({ markdown, sourceFormat }),
    [markdown, sourceFormat],
  );
  const [baseline, setBaseline] = useState<DirtyStateBaseline | null>(null);
  const [currentMarkdown, setCurrentMarkdown] = useState('');

  const editor = useEditor(
    {
      immediatelyRender: false,
      extensions: editorExtensions,
      content: loading ? '' : markdown,
      contentType: 'markdown',
      editable: !loading && !saving,
      editorProps: {
        attributes: {
          class: `document-editor-prosemirror document-editor-prosemirror--${surfaceMode} min-h-[68vh] px-5 py-5 outline-none sm:px-7 sm:py-6 lg:px-8 lg:py-7`,
          spellcheck: surfaceMode === 'prose' ? 'true' : 'false',
          autocapitalize: 'off',
          autocomplete: 'off',
          autocorrect: 'off',
        },
        handleDOMEvents: {
          focus: () => false,
        },
      },
      onUpdate: ({ editor: nextEditor }: { editor: Editor }) => {
        setCurrentMarkdown(nextEditor.getMarkdown());
      },
    },
    [loading, markdown, surfaceMode],
  );

  useEffect(() => {
    if (!editor) {
      return;
    }
    editor.setEditable(!loading && !saving);
  }, [editor, loading, saving]);

  useEffect(() => {
    if (!open || !editor || loading) {
      return;
    }

    const syncTimer = window.setTimeout(() => {
      const editorMarkdown = editor.getMarkdown();
      setBaseline(createEditorBaseline(editorMarkdown));
      setCurrentMarkdown(editorMarkdown);
    }, 0);

    return () => window.clearTimeout(syncTimer);
  }, [editor, loading, markdown, open]);

  useEffect(() => {
    if (!open || !editor || loading) {
      return;
    }

    const focusTimer = window.setTimeout(() => {
      editor.commands.focus('start');
    }, 0);
    return () => window.clearTimeout(focusTimer);
  }, [editor, loading, open]);

  const isDirty = !loading && !saving && isEditorContentDirty(baseline, currentMarkdown);
  const statusState = saving ? 'saving' : error ? 'error' : isDirty ? 'dirty' : 'clean';
  const statusLabel = (() => {
    switch (statusState) {
      case 'saving':
        return t('documents.editor.saving');
      case 'error':
        return t('documents.editor.saveFailedShort');
      case 'dirty':
        return t('documents.editor.unsaved');
      case 'clean':
      default:
        return t('documents.editor.synced');
    }
  })();
  const statusTone = statusState === 'dirty'
    ? 'accent'
    : statusState === 'error'
      ? 'destructive'
      : 'neutral';

  const handleRequestClose = () => {
    if (saving) {
      return;
    }
    if (isDirty && !window.confirm(t('documents.editor.unsavedConfirm'))) {
      return;
    }
    onOpenChange(false);
  };

  const handleSave = () => {
    if (!editor) {
      return;
    }
    onSave(editor.getMarkdown());
  };

  const actions = (
    <div className="flex w-full justify-end">
      <div className="flex flex-col-reverse gap-2 sm:flex-row">
        <Button variant="outline" onClick={handleRequestClose} disabled={saving}>
          {t('documents.cancel')}
        </Button>
        <Button onClick={handleSave} disabled={loading || saving || !editor || Boolean(error && !currentMarkdown)}>
          {saving ? (
            <>
              <Loader2 className="mr-2 h-4 w-4 animate-spin" />
              {t('documents.editor.saving')}
            </>
          ) : (
            t('documents.editor.save')
          )}
        </Button>
      </div>
    </div>
  );

  return (
    <DocumentEditorOverlay
      actions={actions}
      description={`${documentName}${sourceFormat ? ` · ${sourceFormat.toUpperCase()}` : ''}`}
      helperText={t('documents.editor.description')}
      onOpenChange={(nextOpen) => {
        if (nextOpen) {
          onOpenChange(true);
          return;
        }
        handleRequestClose();
      }}
      open={open}
      title={t('documents.editor.title')}
    >
      <div className="flex min-h-0 flex-1 flex-col bg-[radial-gradient(circle_at_top,hsl(var(--primary)/0.06),transparent_28%),linear-gradient(180deg,hsl(var(--surface-sunken)/0.42),hsl(var(--background)))]">
        <div className="border-b bg-background/90 px-4 py-4 backdrop-blur supports-[backdrop-filter]:bg-background/88 sm:px-6 sm:py-5">
          <div className="mx-auto w-full max-w-[94rem]">
            <DocumentEditorToolbar
              editor={editor}
              isDirty={isDirty}
              saving={saving}
              sourceFormat={sourceFormat}
              statusLabel={statusLabel}
              statusTone={statusTone}
              surfaceMode={surfaceMode}
              t={t}
            />
          </div>
        </div>

        <div aria-live="polite" className="sr-only">
          {error ?? ''}
        </div>

        <DocumentEditorCanvas
          currentMarkdown={currentMarkdown}
          documentName={documentName}
          editor={editor}
          error={error}
          loading={loading}
          sourceFormat={sourceFormat}
          statusLabel={statusLabel}
          surfaceMode={surfaceMode}
          t={t}
        />
      </div>
    </DocumentEditorOverlay>
  );
}
