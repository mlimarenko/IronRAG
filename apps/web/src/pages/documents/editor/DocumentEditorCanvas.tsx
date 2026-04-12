import { useEffect, useRef, useState, type ChangeEvent, type ReactNode } from 'react';
import type { TFunction } from 'i18next';
import { Loader2 } from 'lucide-react';
import { EditorContent, type Editor } from '@tiptap/react';

import { ScrollArea } from '@/components/ui/scroll-area';
import { cn } from '@/lib/utils';

import type { EditorSurfaceMode } from './editorSurfaceMode';

type DocumentEditorCanvasProps = {
  currentMarkdown: string;
  documentName: string;
  editor: Editor | null;
  error: string | null;
  loading: boolean;
  sourceFormat?: string;
  statusLabel: string;
  surfaceMode: EditorSurfaceMode;
  t: TFunction;
};

export function DocumentEditorCanvas({
  currentMarkdown,
  documentName,
  editor,
  error,
  loading,
  sourceFormat,
  statusLabel,
  surfaceMode,
  t,
}: DocumentEditorCanvasProps) {
  const tableViewportRef = useRef<HTMLDivElement | null>(null);
  const tableContentRef = useRef<HTMLDivElement | null>(null);
  const [tableScrollState, setTableScrollState] = useState({
    canScrollLeft: false,
    canScrollRight: false,
    contentWidth: 0,
    maxScrollLeft: 0,
    scrollLeft: 0,
    showRail: false,
    viewportWidth: 0,
  });
  const hasEditorContent = Boolean(editor?.getMarkdown().trim());
  const showFatalError = Boolean(error) && !hasEditorContent;
  const editorMarkdown = currentMarkdown || editor?.getMarkdown() || '';
  const codeLines = surfaceMode === 'code' ? extractCodeLines(editorMarkdown) : [];

  useEffect(() => {
    if (surfaceMode !== 'table') {
      setTableScrollState({
        canScrollLeft: false,
        canScrollRight: false,
        contentWidth: 0,
        maxScrollLeft: 0,
        scrollLeft: 0,
        showRail: false,
        viewportWidth: 0,
      });
      return;
    }

    const viewport = tableViewportRef.current;
    const content = tableContentRef.current;
    if (!viewport || !content) {
      return;
    }

    const updateLayout = () => {
      const clientWidth = viewport.clientWidth;
      const scrollWidth = Math.max(content.scrollWidth, viewport.scrollWidth, clientWidth);
      const maxScrollLeft = Math.max(scrollWidth - clientWidth, 0);
      const scrollLeft = Math.min(viewport.scrollLeft, maxScrollLeft);
      const showRail = maxScrollLeft > 2;

      setTableScrollState((previous) => {
        const nextState = {
          canScrollLeft: scrollLeft > 1,
          canScrollRight: scrollLeft < maxScrollLeft - 1,
          contentWidth: scrollWidth,
          maxScrollLeft,
          scrollLeft,
          showRail,
          viewportWidth: clientWidth,
        };
        return previous.canScrollLeft === nextState.canScrollLeft &&
          previous.canScrollRight === nextState.canScrollRight &&
          previous.contentWidth === nextState.contentWidth &&
          previous.maxScrollLeft === nextState.maxScrollLeft &&
          previous.scrollLeft === nextState.scrollLeft &&
          previous.showRail === nextState.showRail &&
          previous.viewportWidth === nextState.viewportWidth
          ? previous
          : nextState;
      });
    };

    viewport.addEventListener('scroll', updateLayout, { passive: true });
    window.addEventListener('resize', updateLayout);

    const ResizeObserverCtor = window.ResizeObserver;
    const resizeObserver = ResizeObserverCtor
      ? new ResizeObserverCtor(() => {
        updateLayout();
      })
      : null;

    resizeObserver?.observe(viewport);
    resizeObserver?.observe(content);

    updateLayout();

    return () => {
      viewport.removeEventListener('scroll', updateLayout);
      window.removeEventListener('resize', updateLayout);
      resizeObserver?.disconnect();
    };
  }, [editorMarkdown, surfaceMode]);

  const handleTableRailChange = (event: ChangeEvent<HTMLInputElement>) => {
    const viewport = tableViewportRef.current;
    if (!viewport) {
      return;
    }
    viewport.scrollLeft = Number(event.target.value);
  };

  if (loading) {
    return (
      <div className="flex min-h-0 flex-1 items-center justify-center p-4 sm:p-6">
        <CanvasStateCard>
          <div className="flex items-center gap-3 text-sm text-muted-foreground">
            <Loader2 className="h-4 w-4 animate-spin" />
            <span>{t('documents.editor.loading')}</span>
          </div>
        </CanvasStateCard>
      </div>
    );
  }

  if (showFatalError) {
    return (
      <div className="flex min-h-0 flex-1 p-4 sm:p-6">
        <CanvasStateCard tone="error">
          <div className="rounded-2xl border border-destructive/20 bg-destructive/5 p-4 text-sm text-destructive">
            {error}
          </div>
        </CanvasStateCard>
      </div>
    );
  }

  if (!editor) {
    return (
      <div className="flex min-h-0 flex-1 items-center justify-center p-4 sm:p-6">
        <CanvasStateCard>
          <span className="text-sm text-muted-foreground">{t('documents.editor.initializing')}</span>
        </CanvasStateCard>
      </div>
    );
  }

  return (
    <div className="flex min-h-0 min-w-0 flex-1 flex-col px-4 py-4 sm:px-6 sm:py-5">
      <div className={cn('mx-auto flex min-h-0 min-w-0 w-full flex-1', frameWidthClassName(surfaceMode))}>
        <div className="flex min-h-0 min-w-0 w-full flex-1 flex-col overflow-hidden rounded-[28px] border border-border/70 bg-background/98 shadow-[0_24px_90px_hsl(var(--foreground)/0.08)]">
          {error ? (
            <div className="border-b border-destructive/20 bg-destructive/5 px-5 py-3 text-sm text-destructive sm:px-6">
              {error}
            </div>
          ) : null}

          {surfaceMode === 'table' ? (
            <div className="flex min-h-0 min-w-0 w-full flex-1 flex-col">
              <div
                ref={tableViewportRef}
                className={cn(
                  'document-editor-table-scroll min-h-0 min-w-0 w-full flex-1 overflow-auto',
                  tableScrollState.canScrollLeft && 'document-editor-table-scroll--can-left',
                  tableScrollState.canScrollRight && 'document-editor-table-scroll--can-right',
                )}
              >
                <div ref={tableContentRef} className="min-h-full w-max min-w-full pb-4">
                  <EditorContent editor={editor} />
                </div>
              </div>

              {tableScrollState.showRail ? (
                <div className="document-editor-table-rail-shell">
                  <input
                    aria-label={t('documents.editor.tableScrollRail')}
                    className="document-editor-table-slider"
                    data-testid="document-editor-table-slider"
                    max={tableScrollState.maxScrollLeft}
                    min={0}
                    onChange={handleTableRailChange}
                    type="range"
                    value={Math.min(tableScrollState.scrollLeft, tableScrollState.maxScrollLeft)}
                  />
                </div>
              ) : null}
            </div>
          ) : (
            <ScrollArea className="min-h-0 flex-1">
              <div className={cn('min-h-full', surfaceScrollWrapperClassName(surfaceMode))}>
                {surfaceMode === 'code' ? (
                  <div className={cn('min-h-full', surfaceContentClassName(surfaceMode))}>
                    <div className="document-editor-code-shell">
                      <div className="document-editor-code-shell__header">
                        <div className="flex items-center gap-2">
                          <span className="font-medium text-foreground">{documentName}</span>
                          {sourceFormat ? (
                            <span className="rounded-full border border-border/80 bg-background px-2 py-0.5 text-[10px] font-semibold uppercase tracking-[0.08em] text-muted-foreground">
                              {sourceFormat}
                            </span>
                          ) : null}
                        </div>
                        <span>{t('documents.editor.codeFooterLines', { count: codeLines.length })}</span>
                      </div>

                      <div className="document-editor-code-shell__body">
                        <div className="document-editor-code-shell__gutter" aria-hidden="true" data-testid="document-editor-code-gutter">
                          {codeLines.map((_, index) => (
                            <span key={index}>{index + 1}</span>
                          ))}
                        </div>

                        <div className="document-editor-code-shell__content">
                          <EditorContent editor={editor} />
                        </div>
                      </div>

                      <div className="document-editor-code-shell__footer">
                        <span>{statusLabel}</span>
                        <span>{t('documents.editor.codeFooterIndent')}</span>
                      </div>
                    </div>
                  </div>
                ) : (
                  <div className={cn('min-h-full', surfaceContentClassName(surfaceMode))}>
                    <EditorContent editor={editor} />
                  </div>
                )}
              </div>
            </ScrollArea>
          )}
        </div>
      </div>
    </div>
  );
}

function frameWidthClassName(surfaceMode: EditorSurfaceMode): string {
  switch (surfaceMode) {
    case 'table':
      return 'max-w-full';
    case 'code':
      return 'max-w-[96rem]';
    case 'prose':
    default:
      return 'max-w-[74rem]';
  }
}

function surfaceScrollWrapperClassName(surfaceMode: EditorSurfaceMode): string {
  switch (surfaceMode) {
    case 'table':
      return 'overflow-x-auto overscroll-x-contain pb-4';
    case 'code':
      return 'overflow-x-auto overscroll-x-contain';
    case 'prose':
    default:
      return '';
  }
}

function surfaceContentClassName(surfaceMode: EditorSurfaceMode): string {
  switch (surfaceMode) {
    case 'table':
      return 'w-max min-w-full';
    case 'code':
      return 'min-w-full';
    case 'prose':
    default:
      return 'mx-auto w-full max-w-[54rem]';
  }
}

function extractCodeLines(markdown: string): string[] {
  const normalized = markdown.replace(/\r\n?/g, '\n');
  const fencedMatch = normalized.match(/^```[^\n]*\n([\s\S]*?)\n```$/);
  const codeText = fencedMatch ? fencedMatch[1] : normalized;
  const lines = codeText.split('\n');
  return lines.length === 1 && lines[0] === '' ? [''] : lines;
}

type CanvasStateCardProps = {
  children: ReactNode;
  tone?: 'default' | 'error';
};

function CanvasStateCard({ children, tone = 'default' }: CanvasStateCardProps) {
  return (
    <div
      className={cn(
        'mx-auto flex min-h-[62vh] w-full max-w-[74rem] items-center justify-center rounded-[28px] border bg-background/96 p-6 shadow-[0_20px_70px_hsl(var(--foreground)/0.08)]',
        tone === 'default' ? 'border-border/70' : 'border-destructive/20',
      )}
    >
      {children}
    </div>
  );
}
