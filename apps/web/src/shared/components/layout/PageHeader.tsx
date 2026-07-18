import type { ReactNode } from 'react'

import { cn } from '@/shared/lib/utils'

type PageHeaderProps = Readonly<{
  title: ReactNode
  description?: ReactNode
  eyebrow?: ReactNode
  actions?: ReactNode
  tabs?: ReactNode
  notice?: ReactNode
  className?: string
  titleClassName?: string
}>

export function PageHeader({
  title,
  description,
  eyebrow,
  actions,
  tabs,
  notice,
  className,
  titleClassName,
}: PageHeaderProps) {
  return (
    <header className={cn('shrink-0 border-b bg-background px-4 py-3 sm:px-6', className)}>
      {notice ? <div className="mb-3">{notice}</div> : null}
      <div className="flex min-h-11 flex-wrap items-center justify-between gap-3">
        <div className="min-w-0">
          {eyebrow ? <div className="section-label text-muted-foreground">{eyebrow}</div> : null}
          <h1 className={cn('truncate text-xl font-bold tracking-tight', titleClassName)}>
            {title}
          </h1>
          {description ? (
            <div className="mt-0.5 max-w-3xl text-sm leading-snug text-muted-foreground">
              {description}
            </div>
          ) : null}
        </div>
        {(tabs || actions) && (
          <div className="flex min-w-0 flex-wrap items-center justify-end gap-2">
            {tabs}
            {actions}
          </div>
        )}
      </div>
    </header>
  )
}
