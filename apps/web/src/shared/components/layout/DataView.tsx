import { useEffect, useId, useRef, useState, type KeyboardEvent, type ReactNode } from 'react'
import { X } from 'lucide-react'

import { Button } from '@/shared/components/ui/button'
import { cn } from '@/shared/lib/utils'

const DOCKED_INSPECTOR_QUERY = '(min-width: 1280px)'
const FOCUSABLE_SELECTOR = [
  'a[href]',
  'button:not([disabled])',
  'textarea:not([disabled])',
  'input:not([disabled])',
  'select:not([disabled])',
  "[tabindex]:not([tabindex='-1'])",
].join(',')

type DataWorkspaceViewProps = Readonly<{
  children: ReactNode
  inspector?: ReactNode
  inspectorCloseLabel?: string
  inspectorLabel?: string
  inspectorOpen?: boolean
  showDrawerHeader?: boolean
  onInspectorOpenChange?: (open: boolean) => void
  className?: string
  mainClassName?: string
}>

function WorkspaceDataView({
  children,
  inspector,
  inspectorCloseLabel = 'Close inspector',
  inspectorLabel = 'Inspector',
  inspectorOpen = false,
  showDrawerHeader = true,
  onInspectorOpenChange,
  className,
  mainClassName,
}: DataWorkspaceViewProps) {
  const inspectorTitleId = useId()
  const inspectorRef = useRef<HTMLDialogElement>(null)
  const restoreFocusRef = useRef<HTMLElement | null>(null)
  const [inspectorDocked, setInspectorDocked] = useState(false)
  const drawerOpen = Boolean(inspector && inspectorOpen && !inspectorDocked)

  useEffect(() => {
    if (typeof window === 'undefined') return
    const query = window.matchMedia(DOCKED_INSPECTOR_QUERY)
    const sync = () => setInspectorDocked(query.matches)
    sync()
    query.addEventListener('change', sync)
    return () => query.removeEventListener('change', sync)
  }, [])

  useEffect(() => {
    if (!drawerOpen) return

    const dialog = inspectorRef.current
    if (dialog && !dialog.open) {
      if (typeof dialog.showModal === 'function') dialog.showModal()
      else dialog.setAttribute('open', '')
    }
    restoreFocusRef.current =
      document.activeElement instanceof HTMLElement ? document.activeElement : null
    const frame = window.requestAnimationFrame(() => {
      const inspector = inspectorRef.current
      const target = inspector?.querySelector<HTMLElement>(FOCUSABLE_SELECTOR) ?? inspector
      target?.focus()
    })

    return () => {
      window.cancelAnimationFrame(frame)
      if (dialog?.open) {
        if (typeof dialog.close === 'function') dialog.close()
        else dialog.removeAttribute('open')
      }
      restoreFocusRef.current?.focus()
      restoreFocusRef.current = null
    }
  }, [drawerOpen])

  const handleInspectorKeyDown = (event: KeyboardEvent<HTMLElement>) => {
    if (!drawerOpen) return

    if (event.key === 'Escape') {
      event.stopPropagation()
      onInspectorOpenChange?.(false)
      return
    }

    if (event.key !== 'Tab') return

    const inspector = inspectorRef.current
    if (!inspector) return
    const focusable = Array.from(
      inspector.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR),
    ).filter((element) => element.offsetParent !== null || element === document.activeElement)

    if (focusable.length === 0) {
      event.preventDefault()
      inspector.focus()
      return
    }

    const first = focusable[0]
    const last = focusable[focusable.length - 1]
    if (!first || !last) return
    if (event.shiftKey && document.activeElement === first) {
      event.preventDefault()
      last.focus()
      return
    }
    if (!event.shiftKey && document.activeElement === last) {
      event.preventDefault()
      first.focus()
    }
  }

  return (
    <div
      className={cn(
        'data-view',
        inspector && inspectorOpen && 'data-view--inspector-open',
        className,
      )}
    >
      <div className={cn('data-view__main', mainClassName)}>{children}</div>
      {inspector && inspectorOpen && (
        <button
          aria-label={inspectorCloseLabel}
          className="data-view__backdrop"
          type="button"
          onClick={() => onInspectorOpenChange?.(false)}
          onKeyDown={(event) => {
            if (event.key === 'Escape') {
              event.stopPropagation()
              onInspectorOpenChange?.(false)
            }
          }}
        />
      )}
      {inspector ? (
        <dialog
          ref={inspectorRef}
          aria-label={drawerOpen && !showDrawerHeader ? inspectorLabel : undefined}
          aria-labelledby={drawerOpen && showDrawerHeader ? inspectorTitleId : undefined}
          aria-modal={drawerOpen || undefined}
          onCancel={(event) => {
            if (!drawerOpen) return
            event.preventDefault()
            onInspectorOpenChange?.(false)
          }}
          className={cn(
            'data-view__inspector',
            inspectorOpen ? 'data-view__inspector--open' : 'data-view__inspector--closed',
          )}
          tabIndex={drawerOpen ? -1 : undefined}
          onKeyDown={handleInspectorKeyDown}
        >
          {showDrawerHeader ? (
            <div className="data-view__drawer-header">
              <div
                id={inspectorTitleId}
                className="min-w-0 truncate section-label text-muted-foreground"
              >
                {inspectorLabel}
              </div>
              <Button
                aria-label={inspectorCloseLabel}
                className="h-8 w-8 shrink-0"
                size="icon"
                type="button"
                variant="ghost"
                onClick={() => onInspectorOpenChange?.(false)}
              >
                <X className="h-4 w-4" />
              </Button>
            </div>
          ) : null}
          <div className="data-view__inspector-body">{inspector}</div>
        </dialog>
      ) : null}
    </div>
  )
}

export { WorkspaceDataView as DataWorkspaceView }
