import type { ReactNode } from 'react'

import { cn } from '@/shared/lib/utils'

/**
 * The canonical status tones. Each maps 1:1 to a `.status-{tone}` utility in
 * index.css (bg + fg + ring already defined as a semantic pair). This is the
 * ONLY vocabulary for conveying status color anywhere in the app — never a raw
 * Tailwind palette literal (`bg-amber-*`, `text-emerald-*`, …) and never an
 * inline `status-badge ${cond ? 'status-ready' : 'status-failed'}` ternary.
 */
export type StatusTone =
  'ready' | 'processing' | 'warning' | 'failed' | 'sparse' | 'queued' | 'stalled'

/**
 * Canonical status pill. Renders `.status-badge .status-{tone}` so every
 * surface (documents, libraries, queue, audit, graph, dashboard) presents a
 * given status identically. Pass a translated label as children — the badge
 * carries no copy of its own.
 */
type StatusBadgeProps = Readonly<{
  tone: StatusTone
  children: ReactNode
  className?: string | undefined
  title?: string | undefined
  'aria-label'?: string | undefined
}>

export function StatusBadge({
  tone,
  children,
  className,
  title,
  'aria-label': ariaLabel,
}: StatusBadgeProps) {
  return (
    <span
      className={cn('status-badge', `status-${tone}`, className)}
      title={title}
      aria-label={ariaLabel}
    >
      {children}
    </span>
  )
}
