import { useEffect, useRef, useState, type ChangeEvent, type ReactNode } from 'react'
import type { TFunction } from 'i18next'
import { Loader2 } from 'lucide-react'
import { EditorContent, type Editor } from '@tiptap/react'

import { ScrollArea } from '@/shared/components/ui/scroll-area'
import { cn } from '@/shared/lib/utils'

import type { EditorSurfaceMode } from './editorSurfaceMode'

type DocumentEditorCanvasProps = Readonly<{
  currentMarkdown: string
  documentName: string
  editor: Editor | null
  error: string | null
  loading: boolean
  onRawTextChange: (markdown: string) => void
  lineWrapEnabled: boolean
  rawTextEditor: boolean
  readOnly?: boolean
  saving: boolean
  sourceFormat?: string | undefined
  sourceHref?: string | undefined
  statusLabel: string
  surfaceMode: EditorSurfaceMode
  t: TFunction
}>

const EMPTY_TABLE_SCROLL_STATE = {
  canScrollLeft: false,
  canScrollRight: false,
  contentWidth: 0,
  maxScrollLeft: 0,
  scrollLeft: 0,
  showRail: false,
  viewportWidth: 0,
}

function updateTableScrollState(
  previous: typeof EMPTY_TABLE_SCROLL_STATE,
  viewport: HTMLDivElement,
  content: HTMLDivElement,
): typeof EMPTY_TABLE_SCROLL_STATE {
  const clientWidth = viewport.clientWidth
  const scrollWidth = Math.max(content.scrollWidth, viewport.scrollWidth, clientWidth)
  const maxScrollLeft = Math.max(scrollWidth - clientWidth, 0)
  const scrollLeft = Math.min(viewport.scrollLeft, maxScrollLeft)
  const nextState = {
    canScrollLeft: scrollLeft > 1,
    canScrollRight: scrollLeft < maxScrollLeft - 1,
    contentWidth: scrollWidth,
    maxScrollLeft,
    scrollLeft,
    showRail: maxScrollLeft > 2,
    viewportWidth: clientWidth,
  }
  return Object.keys(nextState).every(
    (key) => previous[key as keyof typeof previous] === nextState[key as keyof typeof nextState],
  )
    ? previous
    : nextState
}

type EditorSurfaceProps = Readonly<{
  codeLines: string[]
  documentName: string
  editor: Editor
  lineWrapEnabled: boolean
  sourceFormat?: string | undefined
  state: typeof EMPTY_TABLE_SCROLL_STATE
  statusLabel: string
  surfaceMode: EditorSurfaceMode
  t: TFunction
  tableContentRef: React.RefObject<HTMLDivElement | null>
  tableViewportRef: React.RefObject<HTMLDivElement | null>
  onTableRailChange: (event: ChangeEvent<HTMLInputElement>) => void
}>

function EditorSurface({
  codeLines,
  documentName,
  editor,
  lineWrapEnabled,
  sourceFormat,
  state,
  statusLabel,
  surfaceMode,
  t,
  tableContentRef,
  tableViewportRef,
  onTableRailChange,
}: EditorSurfaceProps) {
  if (surfaceMode === 'table') {
    return (
      <div className="flex min-h-0 min-w-0 w-full flex-1 flex-col">
        <div
          ref={tableViewportRef}
          className={cn(
            'document-editor-table-scroll min-h-0 min-w-0 w-full flex-1 overflow-x-hidden overflow-y-auto',
            state.canScrollLeft && 'document-editor-table-scroll--can-left',
            state.canScrollRight && 'document-editor-table-scroll--can-right',
          )}
        >
          <div
            ref={tableContentRef}
            className={cn('min-h-full min-w-full pb-4', lineWrapEnabled ? 'w-full' : 'w-max')}
            data-testid="document-editor-table-content"
          >
            <EditorContent editor={editor} />
          </div>
        </div>
        {!lineWrapEnabled && state.showRail && (
          <div className="document-editor-table-rail-shell">
            <input
              aria-label={t('documents.editor.tableScrollRail')}
              className="document-editor-table-slider"
              data-testid="document-editor-table-slider"
              max={state.maxScrollLeft}
              min={0}
              onChange={onTableRailChange}
              type="range"
              value={Math.min(state.scrollLeft, state.maxScrollLeft)}
            />
          </div>
        )}
      </div>
    )
  }
  if (surfaceMode === 'code') {
    return (
      <ScrollArea className="min-h-0 flex-1">
        <div className={cn('min-h-full', surfaceScrollWrapperClassName(surfaceMode))}>
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
                <div
                  className="document-editor-code-shell__gutter"
                  aria-hidden="true"
                  data-testid="document-editor-code-gutter"
                >
                  {codeLines.map((line, index) => (
                    <span key={`${index + 1}:${line}`}>{index + 1}</span>
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
        </div>
      </ScrollArea>
    )
  }
  return (
    <ScrollArea className="min-h-0 flex-1">
      <div className={cn('min-h-full', surfaceScrollWrapperClassName(surfaceMode))}>
        <div className={cn('min-h-full', surfaceContentClassName(surfaceMode))}>
          <EditorContent editor={editor} />
        </div>
      </div>
    </ScrollArea>
  )
}

export function DocumentEditorCanvas({
  currentMarkdown,
  documentName,
  editor,
  error,
  loading,
  onRawTextChange,
  lineWrapEnabled,
  rawTextEditor,
  readOnly = false,
  saving,
  sourceFormat,
  sourceHref,
  statusLabel,
  surfaceMode,
  t,
}: DocumentEditorCanvasProps) {
  const tableViewportRef = useRef<HTMLDivElement | null>(null)
  const tableContentRef = useRef<HTMLDivElement | null>(null)
  const [tableScrollState, setTableScrollState] = useState(EMPTY_TABLE_SCROLL_STATE)
  const hasEditorContent = Boolean(editor?.getMarkdown().trim())
  const showFatalError = Boolean(error) && !hasEditorContent
  const editorMarkdown = currentMarkdown || editor?.getMarkdown() || ''
  const codeLines = surfaceMode === 'code' ? extractCodeLines(editorMarkdown) : []
  const effectiveTableScrollState =
    surfaceMode === 'table' ? tableScrollState : EMPTY_TABLE_SCROLL_STATE

  useEffect(() => {
    if (surfaceMode !== 'table') {
      return
    }

    const viewport = tableViewportRef.current
    const content = tableContentRef.current
    if (!viewport || !content) {
      return
    }

    const updateLayout = () => {
      if (lineWrapEnabled) {
        if (viewport.scrollLeft !== 0) {
          viewport.scrollLeft = 0
        }
        setTableScrollState(EMPTY_TABLE_SCROLL_STATE)
        return
      }

      setTableScrollState((previous) => updateTableScrollState(previous, viewport, content))
    }

    viewport.addEventListener('scroll', updateLayout, { passive: true })
    window.addEventListener('resize', updateLayout)

    const ResizeObserverCtor = window.ResizeObserver
    const resizeObserver = ResizeObserverCtor
      ? new ResizeObserverCtor(() => {
          updateLayout()
        })
      : null

    resizeObserver?.observe(viewport)
    resizeObserver?.observe(content)

    updateLayout()

    return () => {
      viewport.removeEventListener('scroll', updateLayout)
      window.removeEventListener('resize', updateLayout)
      resizeObserver?.disconnect()
    }
  }, [editorMarkdown, lineWrapEnabled, surfaceMode])

  const handleTableRailChange = (event: ChangeEvent<HTMLInputElement>) => {
    const viewport = tableViewportRef.current
    if (!viewport) {
      return
    }
    viewport.scrollLeft = Number(event.target.value)
  }

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
    )
  }

  if (showFatalError) {
    return (
      <div className="flex min-h-0 flex-1 p-4 sm:p-6">
        <CanvasStateCard tone="error">
          <div className="rounded-xl border border-destructive/20 bg-destructive/5 p-4 text-sm text-destructive">
            {error}
          </div>
        </CanvasStateCard>
      </div>
    )
  }

  if (rawTextEditor) {
    return (
      <div className="flex min-h-0 min-w-0 flex-1 flex-col px-4 py-4 sm:px-6 sm:py-5">
        <div className="mx-auto flex min-h-0 min-w-0 w-full max-w-[96rem] flex-1">
          <div className="document-editor-code-shell">
            {error ? (
              <div className="border-b border-destructive/20 bg-destructive/5 px-5 py-3 text-sm text-destructive sm:px-6">
                {error}
              </div>
            ) : null}

            <div className="document-editor-code-shell__header">
              <div className="flex items-center gap-2">
                <span className="font-medium text-foreground">{documentName}</span>
                {sourceFormat ? (
                  <span className="rounded-full border border-border/80 bg-background px-2 py-0.5 text-[10px] font-semibold uppercase tracking-[0.08em] text-muted-foreground">
                    {sourceFormat}
                  </span>
                ) : null}
              </div>
              <span>{statusLabel}</span>
            </div>

            <textarea
              aria-label={documentName}
              className={cn(
                'document-editor-raw-textarea',
                lineWrapEnabled && 'document-editor-raw-textarea--wrap',
              )}
              disabled={saving}
              onChange={(event) => onRawTextChange(event.target.value)}
              readOnly={readOnly}
              spellCheck={false}
              value={currentMarkdown}
            />

            <div className="document-editor-code-shell__footer">
              <span>{statusLabel}</span>
            </div>
          </div>
        </div>
      </div>
    )
  }

  if (!editor) {
    return (
      <div className="flex min-h-0 flex-1 items-center justify-center p-4 sm:p-6">
        <CanvasStateCard>
          <span className="text-sm text-muted-foreground">
            {t('documents.editor.initializing')}
          </span>
        </CanvasStateCard>
      </div>
    )
  }

  if (readOnly && sourceFormat?.toLowerCase() === 'pdf' && sourceHref) {
    return (
      <div className="flex min-h-0 min-w-0 flex-1 flex-col px-4 py-4 sm:px-6 sm:py-5">
        <div className="mx-auto flex min-h-0 min-w-0 w-full max-w-[96rem] flex-1">
          <iframe
            className="min-h-[68vh] w-full rounded-xl border border-border/70 bg-background shadow-[0_24px_90px_hsl(var(--foreground)/0.08)]"
            src={sourceHref}
            title={documentName}
          />
        </div>
      </div>
    )
  }

  return (
    <div className="flex min-h-0 min-w-0 flex-1 flex-col px-4 py-4 sm:px-6 sm:py-5">
      <div
        className={cn(
          'mx-auto flex min-h-0 min-w-0 w-full flex-1',
          frameWidthClassName(surfaceMode),
        )}
      >
        <div className="flex min-h-0 min-w-0 w-full flex-1 flex-col overflow-hidden rounded-xl border border-border/70 bg-background/98 shadow-[0_24px_90px_hsl(var(--foreground)/0.08)]">
          {error ? (
            <div className="border-b border-destructive/20 bg-destructive/5 px-5 py-3 text-sm text-destructive sm:px-6">
              {error}
            </div>
          ) : null}

          <EditorSurface
            codeLines={codeLines}
            documentName={documentName}
            editor={editor}
            lineWrapEnabled={lineWrapEnabled}
            sourceFormat={sourceFormat}
            state={effectiveTableScrollState}
            statusLabel={statusLabel}
            surfaceMode={surfaceMode}
            t={t}
            tableContentRef={tableContentRef}
            tableViewportRef={tableViewportRef}
            onTableRailChange={handleTableRailChange}
          />
        </div>
      </div>
    </div>
  )
}

function frameWidthClassName(surfaceMode: EditorSurfaceMode): string {
  switch (surfaceMode) {
    case 'table':
      return 'max-w-full'
    case 'code':
      return 'max-w-[96rem]'
    case 'prose':
    default:
      return 'max-w-[74rem]'
  }
}

function surfaceScrollWrapperClassName(surfaceMode: EditorSurfaceMode): string {
  switch (surfaceMode) {
    case 'table':
      return 'overflow-x-auto overscroll-x-contain pb-4'
    case 'code':
      return 'overflow-x-auto overscroll-x-contain'
    case 'prose':
    default:
      return ''
  }
}

function surfaceContentClassName(surfaceMode: EditorSurfaceMode): string {
  switch (surfaceMode) {
    case 'table':
      return 'w-max min-w-full'
    case 'code':
      return 'min-w-full'
    case 'prose':
    default:
      return 'mx-auto w-full max-w-[54rem]'
  }
}

function extractCodeLines(markdown: string): string[] {
  const normalized = markdown.replace(/\r\n?/g, '\n')
  const fencedMatch = /^```[^\n]*\n([\s\S]*?)\n```$/u.exec(normalized)
  const codeText = fencedMatch ? (fencedMatch[1] ?? '') : normalized
  const lines = codeText.split('\n')
  return lines.length === 1 && lines[0] === '' ? [''] : lines
}

type CanvasStateCardProps = Readonly<{
  children: ReactNode
  tone?: 'default' | 'error'
}>

function CanvasStateCard({ children, tone = 'default' }: CanvasStateCardProps) {
  return (
    <div
      className={cn(
        'mx-auto flex min-h-[62vh] w-full max-w-[74rem] items-center justify-center rounded-xl border bg-background/96 p-6 shadow-[0_20px_70px_hsl(var(--foreground)/0.08)]',
        tone === 'default' ? 'border-border/70' : 'border-destructive/20',
      )}
    >
      {children}
    </div>
  )
}
