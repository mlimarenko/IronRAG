import type { ReactNode } from 'react';
import type { TFunction } from 'i18next';
import type { Editor } from '@tiptap/react';

import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { cn } from '@/lib/utils';

import type { EditorSurfaceMode } from './editorSurfaceMode';

type DocumentEditorToolbarProps = {
  documentName: string;
  editor: Editor | null;
  isDirty: boolean;
  saving: boolean;
  sourceFormat?: string;
  statusLabel: string;
  statusTone: 'neutral' | 'accent' | 'destructive';
  surfaceMode: EditorSurfaceMode;
  t: TFunction;
};

export function DocumentEditorToolbar({
  editor,
  isDirty,
  saving,
  sourceFormat,
  statusLabel,
  statusTone,
  surfaceMode,
  t,
}: DocumentEditorToolbarProps) {
  const tableActionsDisabled = !editor || !editor.isActive('table');
  const tableActionTitle = tableActionsDisabled ? t('documents.editor.tableSelectionHint') : undefined;
  const historyDisabled = !editor || saving;
  const showHistory = surfaceMode !== 'raw_text';
  const ribbonActions = actionItems({
    editor,
    saving,
    surfaceMode,
    t,
    tableActionsDisabled,
    tableActionTitle,
  });
  const helperText = helperCopy(surfaceMode, t);
  const showRibbon =
    ribbonActions.primary.length > 0 ||
    ribbonActions.secondary.length > 0 ||
    showHistory ||
    isDirty;

  return (
    <div className="flex flex-col gap-3">
      <div className="flex flex-col gap-3 lg:flex-row lg:items-center lg:justify-between">
        <div className="flex flex-wrap items-center gap-2">
          <Badge variant="secondary" className="rounded-full px-3 py-1 text-[11px] font-semibold">
            {modeLabel(surfaceMode, t)}
          </Badge>
          {sourceFormat ? (
            <Badge variant="outline" className="rounded-full bg-background px-3 py-1 text-[11px] font-semibold uppercase">
              {sourceFormat}
            </Badge>
          ) : null}
          <Badge
            variant={statusTone === 'accent' ? 'default' : statusTone === 'destructive' ? 'destructive' : 'outline'}
            className={cn(
              'rounded-full px-3 py-1 text-[11px] font-semibold',
              statusTone === 'neutral' && 'bg-background text-muted-foreground',
            )}
          >
            {statusLabel}
          </Badge>
        </div>
        {helperText ? (
          <p className="max-w-2xl text-xs text-muted-foreground">
            {helperText}
          </p>
        ) : null}
      </div>

      {showRibbon ? (
        <div className="flex flex-wrap items-center gap-2 rounded-2xl border border-border/70 bg-muted/25 px-3 py-2">
          {ribbonActions.primary.length > 0 ? (
            <ToolbarCluster>{ribbonActions.primary}</ToolbarCluster>
          ) : null}
          {ribbonActions.secondary.length > 0 ? (
            <>
              <ToolbarDivider />
              <ToolbarCluster>{ribbonActions.secondary}</ToolbarCluster>
            </>
          ) : null}
          {showHistory ? (
            <>
              <ToolbarDivider />
              <ToolbarCluster>
                <ToolbarButton
                  disabled={historyDisabled}
                  label={t('documents.editor.undo')}
                  onClick={() => editor?.chain().focus().undo().run()}
                />
                <ToolbarButton
                  disabled={historyDisabled}
                  label={t('documents.editor.redo')}
                  onClick={() => editor?.chain().focus().redo().run()}
                />
              </ToolbarCluster>
            </>
          ) : null}
          {isDirty ? (
            <>
              <ToolbarDivider />
              <p className="text-xs text-muted-foreground">{t('documents.editor.unsavedHint')}</p>
            </>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}

type ToolbarButtonProps = {
  active?: boolean;
  disabled?: boolean;
  label: string;
  onClick: () => void;
  title?: string;
};

function ToolbarButton({
  active = false,
  disabled = false,
  label,
  onClick,
  title,
}: ToolbarButtonProps) {
  return (
    <Button
      size="sm"
      title={title}
      variant={active ? 'default' : 'outline'}
      className={cn(
        'h-8 rounded-full px-3 text-xs',
        !active && 'bg-background text-muted-foreground hover:text-foreground',
      )}
      disabled={disabled}
      onClick={onClick}
      type="button"
    >
      {label}
    </Button>
  );
}

type ToolbarClusterProps = {
  children: ReactNode;
};

function ToolbarCluster({ children }: ToolbarClusterProps) {
  return (
    <div className="flex flex-wrap items-center gap-2">
      {children}
    </div>
  );
}

function ToolbarDivider() {
  return <div className="hidden h-8 w-px bg-border lg:block" />;
}

function modeLabel(surfaceMode: EditorSurfaceMode, t: TFunction): string {
  switch (surfaceMode) {
    case 'table':
      return t('documents.editor.tableMode');
    case 'code':
      return t('documents.editor.codeMode');
    case 'raw_text':
      return t('documents.editor.proseMode');
    case 'prose':
    default:
      return t('documents.editor.proseMode');
  }
}

function helperCopy(surfaceMode: EditorSurfaceMode, t: TFunction): string {
  switch (surfaceMode) {
    case 'table':
      return t('documents.editor.tableScrollHint');
    case 'code':
      return t('documents.editor.codeModeHint');
    case 'raw_text':
      return '';
    case 'prose':
    default:
      return t('documents.editor.description');
  }
}

type ActionItemsOptions = {
  editor: Editor | null;
  saving: boolean;
  surfaceMode: EditorSurfaceMode;
  t: TFunction;
  tableActionsDisabled: boolean;
  tableActionTitle?: string;
};

function actionItems({
  editor,
  saving,
  surfaceMode,
  t,
  tableActionsDisabled,
  tableActionTitle,
}: ActionItemsOptions): { primary: ReactNode[]; secondary: ReactNode[] } {
  const commonTableActions = [
    <ToolbarButton
      key="insert-table"
      disabled={!editor || saving}
      label={t('documents.editor.table')}
      onClick={() => editor?.chain().focus().insertTable({ rows: 3, cols: 3, withHeaderRow: true }).run()}
    />,
    <ToolbarButton
      key="add-row"
      disabled={tableActionsDisabled || saving}
      label={t('documents.editor.row')}
      onClick={() => editor?.chain().focus().addRowAfter().run()}
      title={tableActionTitle}
    />,
    <ToolbarButton
      key="add-column"
      disabled={tableActionsDisabled || saving}
      label={t('documents.editor.column')}
      onClick={() => editor?.chain().focus().addColumnAfter().run()}
      title={tableActionTitle}
    />,
  ];

  switch (surfaceMode) {
    case 'raw_text':
      return {
        primary: [],
        secondary: [],
      };
    case 'table':
      return {
        primary: [
          <ToolbarButton
            key="h1"
            active={editor?.isActive('heading', { level: 1 })}
            disabled={!editor || saving}
            label="H1"
            onClick={() => editor?.chain().focus().toggleHeading({ level: 1 }).run()}
          />,
          <ToolbarButton
            key="h2"
            active={editor?.isActive('heading', { level: 2 })}
            disabled={!editor || saving}
            label="H2"
            onClick={() => editor?.chain().focus().toggleHeading({ level: 2 }).run()}
          />,
        ],
        secondary: commonTableActions,
      };
    case 'code':
      return {
        primary: [
          <ToolbarButton
            key="code"
            active={editor?.isActive('codeBlock')}
            disabled={!editor || saving}
            label={t('documents.editor.code')}
            onClick={() => editor?.chain().focus().toggleCodeBlock().run()}
          />,
        ],
        secondary: [],
      };
    case 'prose':
    default:
      return {
        primary: [
          <ToolbarButton
            key="h1"
            active={editor?.isActive('heading', { level: 1 })}
            disabled={!editor || saving}
            label="H1"
            onClick={() => editor?.chain().focus().toggleHeading({ level: 1 }).run()}
          />,
          <ToolbarButton
            key="h2"
            active={editor?.isActive('heading', { level: 2 })}
            disabled={!editor || saving}
            label="H2"
            onClick={() => editor?.chain().focus().toggleHeading({ level: 2 }).run()}
          />,
          <ToolbarButton
            key="bullets"
            active={editor?.isActive('bulletList')}
            disabled={!editor || saving}
            label={t('documents.editor.bullets')}
            onClick={() => editor?.chain().focus().toggleBulletList().run()}
          />,
          <ToolbarButton
            key="quote"
            active={editor?.isActive('blockquote')}
            disabled={!editor || saving}
            label={t('documents.editor.quote')}
            onClick={() => editor?.chain().focus().toggleBlockquote().run()}
          />,
          <ToolbarButton
            key="code"
            active={editor?.isActive('codeBlock')}
            disabled={!editor || saving}
            label={t('documents.editor.code')}
            onClick={() => editor?.chain().focus().toggleCodeBlock().run()}
          />,
        ],
        secondary: commonTableActions,
      };
  }
}
