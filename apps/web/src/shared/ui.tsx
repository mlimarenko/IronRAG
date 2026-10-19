import * as Dialog from '@radix-ui/react-dialog'
import { AlertTriangle, ChevronRight, Copy, Inbox, LoaderCircle, Search, X } from 'lucide-react'
import { useId, useState, type ButtonHTMLAttributes, type ReactNode } from 'react'
import { toast } from 'sonner'
import { useTranslation } from 'react-i18next'
import type { DemoScenario } from '@/shared/types'

type ButtonProps = ButtonHTMLAttributes<HTMLButtonElement> & {
  variant?: 'primary' | 'secondary' | 'ghost' | 'danger'
  size?: 'sm' | 'md' | 'icon'
}

export function Button({
  variant = 'secondary',
  size = 'md',
  className = '',
  ...props
}: ButtonProps) {
  // `type` defaults to "submit" inside a <form>; every caller that wants that
  // passes type="submit" explicitly, so "button" is the safe default here and
  // the spread still lets a caller override it.
  return (
    <button
      data-slot="button"
      type="button"
      className={`button button--${variant} button--${size} ${className}`}
      {...props}
    />
  )
}

export function IconButton({
  label,
  children,
  ...props
}: ButtonProps & { label: string; children: ReactNode }) {
  return (
    <Button size="icon" variant="ghost" aria-label={label} title={label} {...props}>
      {children}
    </Button>
  )
}

export function Badge({
  children,
  tone = 'neutral',
}: Readonly<{
  children: ReactNode
  tone?: string | undefined
}>) {
  return (
    <span data-slot="badge" className={`badge badge--${tone}`}>
      <span className="badge__label">{children}</span>
    </span>
  )
}

export function Panel({
  children,
  className = '',
}: Readonly<{ children: ReactNode; className?: string }>) {
  return (
    <section data-slot="card" className={`panel ${className}`}>
      {children}
    </section>
  )
}

export function SettingsList({
  label,
  children,
}: Readonly<{ label: string; children: ReactNode }>) {
  return (
    <ul className="settings-list" data-slot="settings-list" aria-label={label}>
      {children}
    </ul>
  )
}

export function SettingsListItem({
  title,
  description,
  summary,
  actions,
  children,
}: Readonly<{
  title: string
  description?: ReactNode
  summary?: ReactNode
  actions?: ReactNode
  children: ReactNode
}>) {
  const [expanded, setExpanded] = useState(false)
  const detailsId = useId()
  return (
    <li className="settings-row" data-slot="settings-row" aria-label={title}>
      <div className="settings-row__identity">
        <button
          className="settings-row__toggle"
          type="button"
          aria-expanded={expanded}
          aria-controls={detailsId}
          onClick={() => setExpanded((current) => !current)}
        >
          <ChevronRight size={14} aria-hidden="true" />
          <span>{title}</span>
        </button>
        {description && <div className="settings-row__description">{description}</div>}
      </div>
      <div className="settings-row__summary">{summary}</div>
      <div className="settings-row__actions">{actions}</div>
      <div id={detailsId} className="settings-row__details" hidden={!expanded}>
        {children}
      </div>
    </li>
  )
}

export function PageHeader({
  eyebrow,
  title,
  description,
  actions,
}: Readonly<{
  eyebrow?: string | undefined
  title: string
  description?: string | undefined
  actions?: ReactNode
}>) {
  return (
    <header data-slot="page-header" className="page-header">
      <div>
        {eyebrow && <div className="eyebrow">{eyebrow}</div>}
        <h1>{title}</h1>
        {description && <p>{description}</p>}
      </div>
      {actions && <div className="page-header__actions">{actions}</div>}
    </header>
  )
}

export function Modal({
  open,
  onOpenChange,
  title,
  description,
  children,
  footer,
  wide = false,
}: Readonly<{
  open: boolean
  onOpenChange: (open: boolean) => void
  title: string
  description?: string | undefined
  children: ReactNode
  footer?: ReactNode
  wide?: boolean
}>) {
  const { t } = useTranslation()
  return (
    <Dialog.Root open={open} onOpenChange={onOpenChange}>
      <Dialog.Portal>
        <Dialog.Overlay className="dialog-overlay" />
        <Dialog.Content className={`dialog-content ${wide ? 'dialog-content--wide' : ''}`}>
          <div className="dialog-heading">
            <div>
              <Dialog.Title>{title}</Dialog.Title>
              {description && <Dialog.Description>{description}</Dialog.Description>}
            </div>
            <Dialog.Close asChild>
              <IconButton label={t('common.close')}>
                <X size={18} />
              </IconButton>
            </Dialog.Close>
          </div>
          <div className="dialog-body">{children}</div>
          {footer && <div className="dialog-footer">{footer}</div>}
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  )
}

export function Drawer({
  open,
  onOpenChange,
  title,
  children,
}: Readonly<{
  open: boolean
  onOpenChange: (open: boolean) => void
  title: string
  children: ReactNode
}>) {
  const { t } = useTranslation()
  return (
    <Dialog.Root open={open} onOpenChange={onOpenChange}>
      <Dialog.Portal>
        <Dialog.Overlay className="dialog-overlay drawer-overlay" />
        <Dialog.Content className="drawer-content">
          <div className="dialog-heading">
            <Dialog.Title>{title}</Dialog.Title>
            <Dialog.Close asChild>
              <IconButton label={t('common.close')}>
                <X size={18} />
              </IconButton>
            </Dialog.Close>
          </div>
          <div className="drawer-body">{children}</div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  )
}

export function CopyButton({
  value,
  label,
  className,
  iconOnly,
}: Readonly<{
  value: string
  label?: string | undefined
  className?: string | undefined
  /** Renders the icon alone, with `label` carried as the accessible name. */
  iconOnly?: boolean | undefined
}>) {
  const { t } = useTranslation()
  const text = label ?? t('common.copy')
  return (
    <Button
      size="sm"
      variant="ghost"
      className={className ?? ''}
      {...(iconOnly ? { title: text, 'aria-label': text } : {})}
      onClick={() => {
        void navigator.clipboard.writeText(value)
        toast.success(t('common.copied'))
      }}
    >
      <Copy size={14} />
      {iconOnly ? null : text}
    </Button>
  )
}

const RESIZER_KEYBOARD_STEP = 24

function clampWidth(value: number, min: number, max: number): number {
  // Rounded: the pointer reports fractions on a scaled display, and a stored
  // 513.5 would come back as a half-pixel column on every later visit.
  return Math.round(Math.min(max, Math.max(min, value)))
}

/**
 * Drag handle for a side panel's width.
 *
 * A separator rather than a decorated border: it takes focus and answers the
 * arrow keys, because a pointer-only affordance leaves the panel stuck at one
 * width for anyone not using a mouse. `edge` says which side the panel sits on,
 * so dragging toward the panel always makes it wider.
 */
export function PanelResizer({
  value,
  min,
  max,
  edge,
  label,
  onChange,
}: Readonly<{
  value: number
  min: number
  max: number
  edge: 'left' | 'right'
  label: string
  onChange: (width: number) => void
}>) {
  const grow = edge === 'right' ? -1 : 1
  return (
    <button
      type="button"
      className="panel-resizer"
      aria-label={label}
      onPointerDown={(event) => {
        event.preventDefault()
        const startX = event.clientX
        const startWidth = value
        // Listening on the window, not the handle: a drag that outruns the
        // 7px strip must keep resizing instead of stopping halfway.
        const move = (moveEvent: PointerEvent) => {
          onChange(clampWidth(startWidth + (moveEvent.clientX - startX) * grow, min, max))
        }
        const stop = () => {
          window.removeEventListener('pointermove', move)
          window.removeEventListener('pointerup', stop)
        }
        window.addEventListener('pointermove', move)
        window.addEventListener('pointerup', stop)
      }}
      onKeyDown={(event) => {
        if (event.key !== 'ArrowLeft' && event.key !== 'ArrowRight') return
        event.preventDefault()
        const direction = event.key === 'ArrowRight' ? 1 : -1
        onChange(clampWidth(value + direction * RESIZER_KEYBOARD_STEP * grow, min, max))
      }}
    />
  )
}

export function ScreenState({
  scenario,
  emptyText,
  emptyHint,
  action,
  errorText,
  onRetry,
  children,
}: Readonly<{
  scenario: DemoScenario
  emptyText?: string
  // What the operator can do next — an empty screen must never be a dead end.
  emptyHint?: string
  action?: ReactNode
  errorText?: string
  onRetry?: () => void
  children: ReactNode
}>) {
  const { t } = useTranslation()
  if (scenario === 'loading')
    return (
      <div className="state-card" aria-live="polite">
        <LoaderCircle className="spin" size={22} />
        <div>
          <strong>{t('common.loading')}</strong>
          <span className="skeleton-line" />
          <span className="skeleton-line short" />
        </div>
      </div>
    )
  if (scenario === 'error')
    return (
      <div className="state-card state-card--error state-card--stacked" role="alert">
        <AlertTriangle size={26} />
        <div>
          <strong>{t('common.error')}</strong>
          <p>{errorText ?? t('common.errorHint')}</p>
          <Button size="sm" onClick={onRetry}>
            {t('common.tryAgain')}
          </Button>
        </div>
      </div>
    )
  if (scenario === 'empty')
    return (
      // Not a checkmark: "nothing here yet" is not a success, and an icon
      // sitting beside the paragraph read as a stray glyph rather than part
      // of the state.
      <div className="state-card state-card--stacked">
        <Inbox size={26} />
        <div>
          <strong>{emptyText ?? t('common.empty')}</strong>
          {emptyHint && <p>{emptyHint}</p>}
          {action}
        </div>
      </div>
    )
  return <>{children}</>
}

export function Segmented({
  value,
  onChange,
  items,
  label,
}: Readonly<{
  value: string
  onChange: (value: string) => void
  items: Array<{ value: string; label: string }>
  label: string
}>) {
  return (
    <div className="segmented" role="group" aria-label={label}>
      {items.map((item) => (
        <button
          type="button"
          key={item.value}
          className={value === item.value ? 'is-active' : ''}
          onClick={() => onChange(item.value)}
        >
          {item.label}
        </button>
      ))}
    </div>
  )
}

export function Tabs<Value extends string>({
  id,
  label,
  value,
  onChange,
  items,
  children,
}: Readonly<{
  id: string
  label: string
  value: Value
  onChange: (value: Value) => void
  items: ReadonlyArray<{ value: Value; label: string; suffix?: ReactNode }>
  children: ReactNode
}>) {
  return (
    <div className="tabs-layout" data-slot="tabs">
      <div className="tabs" data-slot="tabs-list" role="tablist" aria-label={label}>
        {items.map((item, index) => (
          <button
            key={item.value}
            type="button"
            data-slot="tabs-trigger"
            role="tab"
            id={`${id}-tab-${item.value}`}
            aria-controls={`${id}-panel-${item.value}`}
            aria-selected={value === item.value}
            tabIndex={value === item.value ? 0 : -1}
            onClick={() => onChange(item.value)}
            onKeyDown={(event) => {
              let nextIndex: number
              switch (event.key) {
                case 'ArrowRight':
                  nextIndex = (index + 1) % items.length
                  break
                case 'ArrowLeft':
                  nextIndex = (index + items.length - 1) % items.length
                  break
                case 'Home':
                  nextIndex = 0
                  break
                case 'End':
                  nextIndex = items.length - 1
                  break
                default:
                  return
              }
              event.preventDefault()
              const nextItem = items[nextIndex]
              if (!nextItem) return
              onChange(nextItem.value)
              const tabs =
                event.currentTarget.parentElement?.querySelectorAll<HTMLButtonElement>(
                  '[role="tab"]',
                )
              tabs?.[nextIndex]?.focus()
            }}
          >
            {item.label}
            {item.suffix && <span aria-hidden="true">{item.suffix}</span>}
          </button>
        ))}
      </div>
      {items.map((item) => (
        <div
          key={item.value}
          className="tab-panel"
          data-slot="tabs-content"
          role="tabpanel"
          id={`${id}-panel-${item.value}`}
          aria-labelledby={`${id}-tab-${item.value}`}
          hidden={value !== item.value}
          tabIndex={0}
        >
          {value === item.value ? children : null}
        </div>
      ))}
    </div>
  )
}

export function Progress({
  value,
  label,
  tone = 'accent',
}: Readonly<{
  value: number
  label: string
  tone?: 'accent' | 'ready' | 'processing' | 'sparse'
}>) {
  const boundedValue = Math.max(0, Math.min(100, value))
  return (
    <progress
      className={`progress progress--${tone}`}
      data-slot="progress"
      aria-label={label}
      aria-valuenow={boundedValue}
      value={boundedValue}
      max={100}
    />
  )
}

/**
 * Failure card with a retry affordance. Every list screen needs the same
 * "could not load, try again" surface; keeping one copy stops the wording and
 * the markup from drifting apart per screen.
 */
export function ErrorState({ title, onRetry }: Readonly<{ title: string; onRetry?: () => void }>) {
  const { t } = useTranslation()
  return (
    <div className="state-card state-card--error" role="alert">
      <AlertTriangle size={22} />
      <div>
        <strong>{title}</strong>
        {onRetry && (
          <Button size="sm" onClick={onRetry}>
            {t('common.tryAgain')}
          </Button>
        )}
      </div>
    </div>
  )
}

/** Single-line filter input used by every searchable list. */
export function SearchBox({
  value,
  onChange,
  placeholder,
  disabled,
}: Readonly<{
  value: string
  onChange: (value: string) => void
  placeholder?: string
  disabled?: boolean
}>) {
  const { t } = useTranslation()
  return (
    <div className="search-box">
      <Search size={16} />
      <input
        className="input"
        value={value}
        onChange={(event) => onChange(event.target.value)}
        placeholder={placeholder ?? t('common.search')}
        disabled={disabled}
      />
    </div>
  )
}
