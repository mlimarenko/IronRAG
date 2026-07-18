import type { ReactNode } from 'react'

import { cn } from '@/shared/lib/utils'

type PageShellProps = Readonly<{
  header?: ReactNode
  children: ReactNode
  className?: string
  bodyClassName?: string
  bodyScroll?: 'auto' | 'hidden' | 'visible'
}>

function overflowClassFor(bodyScroll: PageShellProps['bodyScroll']): string {
  if (bodyScroll === 'auto') return 'overflow-auto'
  if (bodyScroll === 'visible') return 'overflow-visible'
  return 'overflow-hidden'
}

export function PageShell({
  header,
  children,
  className,
  bodyClassName,
  bodyScroll = 'hidden',
}: PageShellProps) {
  const overflowClass = overflowClassFor(bodyScroll)

  return (
    <div className={cn('flex min-h-0 flex-1 flex-col bg-surface-sunken', className)}>
      {header}
      <div className={cn('min-h-0 flex-1', overflowClass, bodyClassName)}>{children}</div>
    </div>
  )
}
