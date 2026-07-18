import * as React from 'react'

import { cn } from '@/shared/lib/utils'

/**
 * A minimal initials avatar. No image source today (the session exposes only
 * a display name), so this renders a gradient-tinted monogram. Kept in the ui
 * primitive layer so feature surfaces can reuse it. `size` maps to fixed px
 * boxes; `tone` selects the accent gradient.
 */
type AvatarSize = 'sm' | 'md' | 'lg'

const SIZE_CLASSES: Record<AvatarSize, string> = {
  sm: 'h-6 w-6 text-2xs',
  md: 'h-8 w-8 text-xs',
  lg: 'h-10 w-10 text-sm',
}

function initialsFrom(name: string): string {
  const parts = name.trim().split(/\s+/).filter(Boolean)
  if (parts.length === 0) return '?'
  if (parts.length === 1) return parts[0]!.slice(0, 2).toUpperCase()
  return (parts[0]![0]! + parts[parts.length - 1]![0]!).toUpperCase()
}

interface AvatarProps extends React.HTMLAttributes<HTMLSpanElement> {
  name: string
  size?: AvatarSize
}

const Avatar = React.forwardRef<HTMLSpanElement, AvatarProps>(
  ({ name, size = 'md', className, ...props }, ref) => (
    <span
      ref={ref}
      aria-hidden="true"
      className={cn(
        'inline-flex shrink-0 select-none items-center justify-center rounded-full font-bold',
        'bg-[linear-gradient(135deg,hsl(var(--primary)/0.9),hsl(var(--accent-strong)/0.75))]',
        'text-primary-foreground ring-1 ring-inset ring-white/15',
        SIZE_CLASSES[size],
        className,
      )}
      {...props}
    >
      {initialsFrom(name)}
    </span>
  ),
)
Avatar.displayName = 'Avatar'

export { Avatar }
