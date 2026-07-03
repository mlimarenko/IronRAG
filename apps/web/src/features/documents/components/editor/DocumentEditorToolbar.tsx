import type { ReactNode } from 'react';
import type { TFunction } from 'i18next';
import type { Editor } from '@tiptap/react';
import {
  Bold,
  Code2,
  Columns3,
  Heading1,
  Heading2,
  ImageIcon,
  Italic,
  Link,
  Link2Off,
  List,
  ListOrdered,
  Quote,
  Redo2,
  Rows3,
  Table2,
  TextWrap,
  Undo2,
  type LucideIcon,
} from 'lucide-react';

import { Badge } from '@/shared/components/ui/badge';
import { Button } from '@/shared/components/ui/button';
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from '@/shared/components/ui/tooltip';
import { cn } from '@/shared/lib/utils';

import type { EditorSurfaceMode } from './editorSurfaceMode';

type DocumentEditorToolbarProps = {
  editor: Editor | null;
  isDirty: boolean;
  lineWrapEnabled: boolean;
  onLineWrapChange: (enabled: boolean) => void;
  saving: boolean;
  sourceFormat?: string | undefined;
  statusLabel: string;
  statusTone: 'neutral' | 'accent' | 'destructive';
  surfaceMode: EditorSurfaceMode;
  t: TFunction;
};

export function DocumentEditorToolbar({
  editor,
  isDirty,
  lineWrapEnabled,
  onLineWrapChange,
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
    lineWrapEnabled,
    onLineWrapChange,
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
        <TooltipProvider delayDuration={180}>
          <div className="flex flex-wrap items-center gap-2 rounded-xl border border-border/70 bg-muted/25 px-3 py-2">
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
                    icon={Undo2}
                    label={t('documents.editor.undo')}
                    onClick={() => editor?.chain().focus().undo().run()}
                  />
                  <ToolbarButton
                    disabled={historyDisabled}
                    icon={Redo2}
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
        </TooltipProvider>
      ) : null}
    </div>
  );
}

type ToolbarButtonProps = {
  active?: boolean | undefined;
  disabled?: boolean;
  icon?: LucideIcon;
  label: string;
  onClick: () => void;
  title?: string | undefined;
};

function ToolbarButton({
  active = false,
  disabled = false,
  icon: Icon,
  label,
  onClick,
  title,
}: ToolbarButtonProps) {
  const button = (
    <Button
      aria-label={label}
      size="sm"
      title={title}
      variant={active ? 'default' : 'outline'}
      className={cn(
        Icon ? 'h-8 w-8 rounded-full p-0' : 'h-8 rounded-full px-3 text-xs',
        !active && 'bg-background text-muted-foreground hover:text-foreground',
      )}
      disabled={disabled}
      onClick={onClick}
      type="button"
    >
      {Icon ? <Icon className="h-4 w-4" aria-hidden="true" /> : label}
    </Button>
  );

  return (
    <Tooltip>
      <TooltipTrigger asChild>{button}</TooltipTrigger>
      <TooltipContent>{title ?? label}</TooltipContent>
    </Tooltip>
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
  lineWrapEnabled: boolean;
  onLineWrapChange: (enabled: boolean) => void;
  surfaceMode: EditorSurfaceMode;
  t: TFunction;
  tableActionsDisabled: boolean;
  tableActionTitle?: string | undefined;
};

function actionItems({
  editor,
  lineWrapEnabled,
  onLineWrapChange,
  saving,
  surfaceMode,
  t,
  tableActionsDisabled,
  tableActionTitle,
}: ActionItemsOptions): { primary: ReactNode[]; secondary: ReactNode[] } {
  const editableActionDisabled = !editor || saving;
  const wrapAction = (
    <ToolbarButton
      key="line-wrap"
      active={lineWrapEnabled}
      icon={TextWrap}
      label={t('documents.editor.lineWrap')}
      onClick={() => onLineWrapChange(!lineWrapEnabled)}
    />
  );
  const richTextActions = [
    <ToolbarButton
      key="h1"
      active={editor?.isActive('heading', { level: 1 })}
      disabled={editableActionDisabled}
      icon={Heading1}
      label="H1"
      onClick={() => editor?.chain().focus().toggleHeading({ level: 1 }).run()}
    />,
    <ToolbarButton
      key="h2"
      active={editor?.isActive('heading', { level: 2 })}
      disabled={editableActionDisabled}
      icon={Heading2}
      label="H2"
      onClick={() => editor?.chain().focus().toggleHeading({ level: 2 }).run()}
    />,
    <ToolbarButton
      key="bold"
      active={editor?.isActive('bold')}
      disabled={editableActionDisabled}
      icon={Bold}
      label={t('documents.editor.bold')}
      onClick={() => editor?.chain().focus().toggleBold().run()}
    />,
    <ToolbarButton
      key="italic"
      active={editor?.isActive('italic')}
      disabled={editableActionDisabled}
      icon={Italic}
      label={t('documents.editor.italic')}
      onClick={() => editor?.chain().focus().toggleItalic().run()}
    />,
    <ToolbarButton
      key="bullets"
      active={editor?.isActive('bulletList')}
      disabled={editableActionDisabled}
      icon={List}
      label={t('documents.editor.bullets')}
      onClick={() => editor?.chain().focus().toggleBulletList().run()}
    />,
    <ToolbarButton
      key="ordered-list"
      active={editor?.isActive('orderedList')}
      disabled={editableActionDisabled}
      icon={ListOrdered}
      label={t('documents.editor.orderedList')}
      onClick={() => editor?.chain().focus().toggleOrderedList().run()}
    />,
    <ToolbarButton
      key="quote"
      active={editor?.isActive('blockquote')}
      disabled={editableActionDisabled}
      icon={Quote}
      label={t('documents.editor.quote')}
      onClick={() => editor?.chain().focus().toggleBlockquote().run()}
    />,
    <ToolbarButton
      key="code"
      active={editor?.isActive('codeBlock')}
      disabled={editableActionDisabled}
      icon={Code2}
      label={t('documents.editor.code')}
      onClick={() => editor?.chain().focus().toggleCodeBlock().run()}
    />,
  ];
  const richInsertActions = [
    <ToolbarButton
      key="link"
      active={editor?.isActive('link')}
      disabled={editableActionDisabled}
      icon={Link}
      label={t('documents.editor.link')}
      onClick={() => promptForLink(editor, t)}
    />,
    <ToolbarButton
      key="unlink"
      disabled={editableActionDisabled || !editor?.isActive('link')}
      icon={Link2Off}
      label={t('documents.editor.removeLink')}
      onClick={() => editor?.chain().focus().unsetLink().run()}
    />,
    <ToolbarButton
      key="image"
      disabled={editableActionDisabled}
      icon={ImageIcon}
      label={t('documents.editor.image')}
      onClick={() => promptForImage(editor, t)}
    />,
  ];
  const commonTableActions = [
    <ToolbarButton
      key="insert-table"
      disabled={editableActionDisabled}
      icon={Table2}
      label={t('documents.editor.table')}
      onClick={() => editor?.chain().focus().insertTable({ rows: 3, cols: 3, withHeaderRow: true }).run()}
    />,
    <ToolbarButton
      key="add-row"
      disabled={tableActionsDisabled || saving}
      icon={Rows3}
      label={t('documents.editor.row')}
      onClick={() => editor?.chain().focus().addRowAfter().run()}
      title={tableActionTitle}
    />,
    <ToolbarButton
      key="add-column"
      disabled={tableActionsDisabled || saving}
      icon={Columns3}
      label={t('documents.editor.column')}
      onClick={() => editor?.chain().focus().addColumnAfter().run()}
      title={tableActionTitle}
    />,
  ];

  switch (surfaceMode) {
    case 'raw_text':
      return {
        primary: [wrapAction],
        secondary: [],
      };
    case 'table':
      return {
        primary: [wrapAction, ...richTextActions],
        secondary: [...richInsertActions, ...commonTableActions],
      };
    case 'code':
      return {
        primary: [
          wrapAction,
          <ToolbarButton
            key="code"
            active={editor?.isActive('codeBlock')}
            disabled={editableActionDisabled}
            icon={Code2}
            label={t('documents.editor.code')}
            onClick={() => editor?.chain().focus().toggleCodeBlock().run()}
          />,
        ],
        secondary: [],
      };
    case 'prose':
    default:
      return {
        primary: [wrapAction, ...richTextActions],
        secondary: [...richInsertActions, ...commonTableActions],
      };
  }
}

function promptForLink(editor: Editor | null, t: TFunction) {
  if (!editor) {
    return;
  }

  const currentHref = typeof editor.getAttributes('link').href === 'string'
    ? editor.getAttributes('link').href
    : '';
  const href = window.prompt(t('documents.editor.linkPrompt'), currentHref);
  if (href === null) {
    return;
  }

  const normalizedHref = href.trim();
  if (!normalizedHref) {
    editor.chain().focus().extendMarkRange('link').unsetLink().run();
    return;
  }

  if (editor.state.selection.empty) {
    editor.chain().focus().insertContent({
      type: 'text',
      text: normalizedHref,
      marks: [
        {
          type: 'link',
          attrs: { href: normalizedHref },
        },
      ],
    }).run();
    return;
  }

  editor.chain().focus().extendMarkRange('link').setLink({ href: normalizedHref }).run();
}

function promptForImage(editor: Editor | null, t: TFunction) {
  if (!editor) {
    return;
  }

  const src = window.prompt(t('documents.editor.imagePrompt'));
  if (src === null) {
    return;
  }

  const normalizedSrc = src.trim();
  if (!normalizedSrc) {
    return;
  }

  editor.chain().focus().setImage({ src: normalizedSrc }).run();
}
