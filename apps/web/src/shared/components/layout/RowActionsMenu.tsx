import type { ReactNode } from 'react'
import { MoreHorizontal } from 'lucide-react'

import { Button } from '@/shared/components/ui/button'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/shared/components/ui/dropdown-menu'
import { cn } from '@/shared/lib/utils'

export type RowAction = {
  key: string
  label: ReactNode
  icon?: ReactNode
  onSelect: () => void
  destructive?: boolean
  disabled?: boolean
}

type RowActionsMenuProps = Readonly<{
  actions: RowAction[]
  label: string
  align?: 'start' | 'center' | 'end'
  className?: string
}>

export function RowActionsMenu({ actions, label, align = 'end', className }: RowActionsMenuProps) {
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button
          aria-label={label}
          className={cn('h-8 w-8 p-0', className)}
          size="icon"
          type="button"
          variant="outline"
          onClick={(event) => event.stopPropagation()}
        >
          <MoreHorizontal className="h-4 w-4" />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align={align} className="min-w-44">
        {actions.map((action) => (
          <DropdownMenuItem
            key={action.key}
            className={cn('gap-2', action.destructive && 'text-destructive focus:text-destructive')}
            {...(action.disabled !== undefined ? { disabled: action.disabled } : {})}
            onSelect={(event) => {
              event.preventDefault()
              action.onSelect()
            }}
          >
            {action.icon ? <span className="text-muted-foreground">{action.icon}</span> : null}
            <span>{action.label}</span>
          </DropdownMenuItem>
        ))}
      </DropdownMenuContent>
    </DropdownMenu>
  )
}
