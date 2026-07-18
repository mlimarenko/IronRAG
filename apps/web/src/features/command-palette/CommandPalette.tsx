import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useNavigate } from 'react-router-dom'
import * as DialogPrimitive from '@radix-ui/react-dialog'
import {
  Building2,
  Code2,
  CornerDownLeft,
  FileText,
  Home,
  Library as LibraryIcon,
  type LucideIcon,
  MessageSquare,
  Moon,
  Search,
  Settings,
  Share2,
  Sun,
} from 'lucide-react'

import { cn } from '@/shared/lib/utils'
import { useApp } from '@/shared/contexts/app-context'
import { useCan } from '@/shared/auth/useCan'
import { usePreferences } from '@/shared/contexts/preferences-context'
import { fuzzyMatch } from './fuzzy'

type CommandGroup = 'navigation' | 'actions' | 'library' | 'workspace'

interface Command {
  id: string
  group: CommandGroup
  /** Pre-translated visible label, used both for display and fuzzy matching. */
  label: string
  /** Extra keywords folded into the match haystack (also translated). */
  keywords: string
  icon: LucideIcon
  /** Optional trailing hint (e.g. current theme). */
  hint?: string
  run: () => void
}

const GROUP_ORDER: CommandGroup[] = ['navigation', 'actions', 'library', 'workspace']

/**
 * Global ⌘K / Ctrl-K command palette. Keyboard-first navigation hub: fuzzy
 * jump to the four operator surfaces (+ Admin when permitted), quick
 * preference toggles, and active workspace/library switching. Built on the
 * Radix Dialog primitive (same one the shell already uses) so focus trap,
 * scroll-lock, portal and Escape handling come for free; this component only
 * adds list navigation + the fuzzy filter on top.
 *
 * Open/close is owned by the AppShell (it wires the keyboard shortcut and the
 * `open-command-palette` shell intent), keeping the mount surgical.
 */
type CommandPaletteProps = Readonly<{
  open: boolean
  onOpenChange: (open: boolean) => void
}>

export function CommandPalette({ open, onOpenChange }: CommandPaletteProps) {
  const { t } = useTranslation()
  const navigate = useNavigate()
  const {
    workspaces,
    activeWorkspace,
    libraries,
    activeLibrary,
    setActiveWorkspace,
    setActiveLibrary,
  } = useApp()
  const { can } = useCan()
  const { resolvedTheme, cycleTheme, developerMode, toggleDeveloperMode } = usePreferences()

  const [query, setQuery] = useState('')
  const [activeIndex, setActiveIndex] = useState(0)
  const listRef = useRef<HTMLDivElement>(null)
  const inputRef = useRef<HTMLInputElement>(null)

  const close = useCallback(() => onOpenChange(false), [onOpenChange])
  const navigateAndClose = useCallback(
    (path: string) => {
      navigate(path).catch(() => undefined)
      close()
    },
    [close, navigate],
  )

  // Build the full command set. Memoized on the inputs that change its
  // contents so typing (which only changes `query`) never rebuilds it.
  const commands = useMemo<Command[]>(() => {
    const list: Command[] = [
      {
        id: 'nav-home',
        group: 'navigation',
        label: t('nav.home'),
        keywords: t('command.kw.home'),
        icon: Home,
        run: () => navigateAndClose('/dashboard'),
      },
      {
        id: 'nav-documents',
        group: 'navigation',
        label: t('nav.documents'),
        keywords: t('command.kw.documents'),
        icon: FileText,
        run: () => navigateAndClose('/documents'),
      },
      {
        id: 'nav-graph',
        group: 'navigation',
        label: t('nav.graph'),
        keywords: t('command.kw.graph'),
        icon: Share2,
        run: () => navigateAndClose('/graph'),
      },
      {
        id: 'nav-assistant',
        group: 'navigation',
        label: t('nav.assistant'),
        keywords: t('command.kw.assistant'),
        icon: MessageSquare,
        run: () => navigateAndClose('/assistant'),
      },
    ]

    if (can('admin.access')) {
      list.push({
        id: 'nav-admin',
        group: 'navigation',
        label: t('nav.admin'),
        keywords: t('command.kw.admin'),
        icon: Settings,
        run: () => navigateAndClose('/admin'),
      })
    }

    // Quick actions.
    if (activeLibrary?.queryReady) {
      list.push({
        id: 'action-ask',
        group: 'actions',
        label: t('command.action.newQuestion'),
        keywords: t('command.kw.ask'),
        icon: MessageSquare,
        run: () => navigateAndClose('/assistant'),
      })
    }
    if (can('content.upload')) {
      list.push({
        id: 'action-add-content',
        group: 'actions',
        label: t('command.action.addContent'),
        keywords: t('command.kw.addContent'),
        icon: FileText,
        run: () => navigateAndClose('/documents'),
      })
    }
    list.push({
      id: 'action-toggle-theme',
      group: 'actions',
      label: t('command.action.toggleTheme'),
      keywords: t('command.kw.theme'),
      icon: resolvedTheme === 'dark' ? Sun : Moon,
      hint: t(`shell.theme${resolvedTheme === 'dark' ? 'Dark' : 'Light'}`),
      run: () => {
        cycleTheme()
        close()
      },
    })
    if (can('devmode.toggle')) {
      list.push({
        id: 'action-toggle-devmode',
        group: 'actions',
        label: t('command.action.toggleDevMode'),
        keywords: t('command.kw.devMode'),
        icon: Code2,
        hint: developerMode ? t('command.on') : t('command.off'),
        run: () => {
          toggleDeveloperMode()
          close()
        },
      })
    }

    // Library switching (other libraries in the active workspace).
    for (const library of libraries) {
      if (library.id === activeLibrary?.id) continue
      list.push({
        id: `library-${library.id}`,
        group: 'library',
        label: library.name,
        keywords: t('command.kw.switchLibrary'),
        icon: LibraryIcon,
        run: () => {
          setActiveLibrary(library)
          close()
        },
      })
    }

    // Workspace switching (other workspaces).
    for (const workspace of workspaces) {
      if (workspace.id === activeWorkspace?.id) continue
      list.push({
        id: `workspace-${workspace.id}`,
        group: 'workspace',
        label: workspace.name,
        keywords: t('command.kw.switchWorkspace'),
        icon: Building2,
        run: () => {
          setActiveWorkspace(workspace)
          close()
        },
      })
    }

    return list
  }, [
    t,
    close,
    navigateAndClose,
    can,
    activeLibrary,
    libraries,
    workspaces,
    activeWorkspace,
    resolvedTheme,
    developerMode,
    cycleTheme,
    toggleDeveloperMode,
    setActiveLibrary,
    setActiveWorkspace,
  ])

  // Filter + rank. With no query, show everything in declared order.
  const filtered = useMemo(() => {
    const trimmed = query.trim()
    if (!trimmed) return commands
    const scored: { command: Command; score: number }[] = []
    for (const command of commands) {
      const haystack = `${command.label} ${command.keywords}`
      const result = fuzzyMatch(trimmed, haystack)
      if (result) scored.push({ command, score: result.score })
    }
    scored.sort((a, b) => b.score - a.score)
    return scored.map((entry) => entry.command)
  }, [commands, query])

  // Group while preserving the ranked order within each group.
  const grouped = useMemo(() => {
    const buckets = new Map<CommandGroup, Command[]>()
    for (const command of filtered) {
      const bucket = buckets.get(command.group) ?? []
      bucket.push(command)
      buckets.set(command.group, bucket)
    }
    const ordered: { group: CommandGroup; commands: Command[] }[] = []
    for (const group of GROUP_ORDER) {
      const bucket = buckets.get(group)
      if (bucket && bucket.length > 0) ordered.push({ group, commands: bucket })
    }
    return ordered
  }, [filtered])

  // Flat order matches what the eye sees, so arrow-key index maps 1:1.
  const flatCommands = useMemo(() => grouped.flatMap((section) => section.commands), [grouped])

  // Clamp during render rather than via an effect: deriving the safe index
  // avoids a synchronous setState-in-effect cascade when the filtered list
  // shrinks under the cursor. `activeIndex` is the user's *intent*; this is
  // what actually highlights.
  const safeIndex = flatCommands.length === 0 ? 0 : Math.min(activeIndex, flatCommands.length - 1)

  // Focus the search field when the palette opens (Radix returns focus to the
  // trigger on close). A managed focus avoids the autoFocus attribute.
  useEffect(() => {
    if (!open) return
    const id = window.requestAnimationFrame(() => inputRef.current?.focus())
    return () => window.cancelAnimationFrame(id)
  }, [open])

  // Scroll the active row into view as the user arrows through. (Reading +
  // calling a DOM method is exactly what an effect is for — no setState here.)
  useEffect(() => {
    if (!open) return
    const node = listRef.current?.querySelector<HTMLElement>(`[data-command-index="${safeIndex}"]`)
    node?.scrollIntoView({ block: 'nearest' })
  }, [safeIndex, open])

  const runActive = useCallback(() => {
    const command = flatCommands[safeIndex]
    command?.run()
  }, [flatCommands, safeIndex])

  const handleKeyDown = useCallback(
    (event: React.KeyboardEvent) => {
      if (event.key === 'ArrowDown') {
        event.preventDefault()
        setActiveIndex((index) =>
          flatCommands.length === 0 ? 0 : (index + 1) % flatCommands.length,
        )
      } else if (event.key === 'ArrowUp') {
        event.preventDefault()
        setActiveIndex((index) =>
          flatCommands.length === 0 ? 0 : (index - 1 + flatCommands.length) % flatCommands.length,
        )
      } else if (event.key === 'Enter') {
        event.preventDefault()
        runActive()
      }
    },
    [flatCommands.length, runActive],
  )

  // Reset transient state on every open/close transition. Doing this in the
  // change handler (an event, not an effect) keeps the reset off the render
  // path entirely.
  const handleOpenChange = useCallback(
    (next: boolean) => {
      if (next) {
        setQuery('')
        setActiveIndex(0)
      }
      onOpenChange(next)
    },
    [onOpenChange],
  )

  return (
    <DialogPrimitive.Root open={open} onOpenChange={handleOpenChange}>
      <DialogPrimitive.Portal>
        <DialogPrimitive.Overlay className="fixed inset-0 z-50 bg-black/60 backdrop-blur-sm data-[state=open]:animate-in data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=open]:fade-in-0" />
        <DialogPrimitive.Content
          onKeyDown={handleKeyDown}
          aria-label={t('command.title')}
          className="fixed left-1/2 top-[12vh] z-50 w-[min(40rem,calc(100vw-2rem))] -translate-x-1/2 overflow-hidden rounded-2xl border bg-popover p-0 text-popover-foreground shadow-overlay data-[state=open]:animate-in data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=open]:fade-in-0 data-[state=closed]:zoom-out-95 data-[state=open]:zoom-in-95"
        >
          <DialogPrimitive.Title className="sr-only">{t('command.title')}</DialogPrimitive.Title>
          <DialogPrimitive.Description className="sr-only">
            {t('command.description')}
          </DialogPrimitive.Description>

          {/* Search field */}
          <div className="flex items-center gap-2.5 border-b px-4">
            <Search className="h-4 w-4 shrink-0 text-muted-foreground" />
            <input
              ref={inputRef}
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              placeholder={t('command.placeholder')}
              className="h-12 flex-1 bg-transparent text-sm outline-none placeholder:text-muted-foreground"
              role="combobox"
              aria-expanded
              aria-controls="command-palette-list"
              aria-activedescendant={
                flatCommands[safeIndex] ? `command-${flatCommands[safeIndex].id}` : undefined
              }
            />
            <kbd className="hidden rounded border bg-muted px-1.5 py-0.5 text-[10px] font-medium text-muted-foreground sm:inline-block">
              ESC
            </kbd>
          </div>

          {/* Results */}
          <select
            id="command-palette-list"
            className="sr-only"
            size={Math.max(2, Math.min(flatCommands.length, 12))}
            value={flatCommands[safeIndex]?.id ?? ''}
            aria-label={t('command.title')}
            onChange={(event) => {
              const commandIndex = flatCommands.findIndex(
                (command) => command.id === event.currentTarget.value,
              )
              if (commandIndex >= 0) setActiveIndex(commandIndex)
            }}
          >
            {grouped.map((section) => (
              <optgroup key={section.group} label={t(`command.groups.${section.group}`)}>
                {section.commands.map((command) => (
                  <option key={command.id} value={command.id}>
                    {command.label}
                  </option>
                ))}
              </optgroup>
            ))}
          </select>
          <div ref={listRef} className="max-h-[min(24rem,60vh)] overflow-y-auto p-2">
            {flatCommands.length === 0 ? (
              <div className="px-3 py-8 text-center text-sm text-muted-foreground">
                {t('command.noResults')}
              </div>
            ) : (
              grouped.map((section) => (
                <div key={section.group} className="mb-1 last:mb-0">
                  <div className="px-2 py-1.5 text-[10px] font-bold uppercase tracking-[0.1em] text-muted-foreground">
                    {t(`command.groups.${section.group}`)}
                  </div>
                  {section.commands.map((command) => {
                    const index = flatCommands.indexOf(command)
                    const isActive = index === safeIndex
                    const Icon = command.icon
                    return (
                      <button
                        key={command.id}
                        id={`command-${command.id}`}
                        type="button"
                        data-command-index={index}
                        onMouseMove={() => setActiveIndex(index)}
                        onClick={() => command.run()}
                        className={cn(
                          'flex w-full items-center gap-3 rounded-lg px-2.5 py-2 text-left text-sm outline-none transition-colors',
                          isActive
                            ? 'bg-accent text-accent-foreground'
                            : 'text-foreground hover:bg-accent/50',
                        )}
                      >
                        <Icon className="h-4 w-4 shrink-0 text-muted-foreground" />
                        <span className="min-w-0 flex-1 truncate">{command.label}</span>
                        {command.hint && (
                          <span className="shrink-0 text-xs text-muted-foreground">
                            {command.hint}
                          </span>
                        )}
                        {isActive && (
                          <CornerDownLeft className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
                        )}
                      </button>
                    )
                  })}
                </div>
              ))
            )}
          </div>

          {/* Footer hint */}
          <div className="flex items-center justify-between gap-3 border-t px-4 py-2 text-[11px] text-muted-foreground">
            <span className="flex items-center gap-1.5">
              {activeWorkspace && (
                <span className="truncate">
                  {activeWorkspace.name}
                  {activeLibrary && (
                    <>
                      <span className="mx-1 text-border">/</span>
                      {activeLibrary.name}
                    </>
                  )}
                </span>
              )}
            </span>
            <span className="hidden shrink-0 items-center gap-2 sm:flex">
              <span className="flex items-center gap-1">
                <kbd className="rounded border bg-muted px-1 py-0.5">↑↓</kbd>
                {t('command.navigate')}
              </span>
              <span className="flex items-center gap-1">
                <kbd className="rounded border bg-muted px-1 py-0.5">↵</kbd>
                {t('command.select')}
              </span>
            </span>
          </div>
        </DialogPrimitive.Content>
      </DialogPrimitive.Portal>
    </DialogPrimitive.Root>
  )
}
