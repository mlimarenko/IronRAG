import { useEffect, useMemo, useRef, useState } from 'react'
import { Code2, Command, FilePlus2, Moon, Search, type LucideIcon } from 'lucide-react'
import { useNavigate } from 'react-router-dom'
import { useTranslation } from 'react-i18next'
import { useApp } from '@/shared/contexts/app-context'
import { adminNav, mainNav } from './navigation'
import { usePreferences } from './preferences-context'
import { Badge, Modal } from '@/shared/ui'

type PaletteCommand = {
  label: string
  category?: string
  state?: string
  stateTone?: 'neutral' | 'ready'
  icon: LucideIcon
  run: () => void | Promise<void>
}

export function CommandPalette({
  open,
  onOpenChange,
}: Readonly<{
  open: boolean
  onOpenChange: (open: boolean) => void
}>) {
  const { t } = useTranslation()
  const navigate = useNavigate()
  const { role, theme, setTheme, devMode, setDevMode } = usePreferences()
  const { workspaces, libraries, setActiveWorkspace, setActiveLibrary } = useApp()
  const [query, setQuery] = useState('')
  const [active, setActive] = useState(0)
  const inputRef = useRef<HTMLInputElement>(null)
  const commands = useMemo(() => {
    const themeLabel = {
      dark: t('common.themeDark'),
      light: t('common.themeLight'),
      system: t('common.themeSystem'),
    }[theme]
    // Derived from the sidebar tables rather than restated here: a second copy
    // drifted, and the palette ended up offering the assistant under a
    // different icon than the sidebar while reaching only one of the eight
    // administration screens.
    const core: PaletteCommand[] = [
      ...mainNav.map(([route, labelKey, icon]) => ({
        label: t(labelKey),
        category: t('shell.navigation'),
        icon,
        run: () => navigate(route),
      })),
      {
        label: t('documents.add'),
        category: t('nav.documents'),
        icon: FilePlus2,
        run: () => navigate('/documents?upload=1'),
      },
      {
        label: t('shell.devMode'),
        state: devMode ? t('common.on') : t('common.off'),
        stateTone: devMode ? 'ready' : 'neutral',
        icon: Code2,
        run: () => setDevMode(!devMode),
      },
      {
        label: t('common.theme'),
        state: themeLabel,
        stateTone: 'neutral',
        icon: Moon,
        run: () => setTheme(theme === 'dark' ? 'light' : 'dark'),
      },
    ]
    if (role === 'admin')
      core.push(
        ...adminNav.flatMap(([groupLabelKey, entries]) =>
          entries.map(([route, labelKey, icon]) => ({
            label: t(labelKey),
            category: `${t('nav.admin')} · ${t(groupLabelKey)}`,
            icon,
            run: () => navigate(route),
          })),
        ),
      )
    const workspaceCommands: PaletteCommand[] = workspaces.map((workspace) => ({
      label: workspace.name,
      category: t('common.workspace'),
      icon: Command,
      run: () => setActiveWorkspace(workspace),
    }))
    const libraryCommands: PaletteCommand[] = libraries.map((library) => ({
      label: library.name,
      category: t('common.library'),
      icon: Command,
      run: () => setActiveLibrary(library),
    }))
    return [...core, ...workspaceCommands, ...libraryCommands]
  }, [
    t,
    navigate,
    role,
    theme,
    setTheme,
    devMode,
    setDevMode,
    workspaces,
    libraries,
    setActiveWorkspace,
    setActiveLibrary,
  ])
  const filtered = commands.filter((item) =>
    `${item.label} ${item.category ?? ''} ${item.state ?? ''}`
      .toLowerCase()
      .includes(query.toLowerCase()),
  )

  useEffect(() => {
    if (open) window.setTimeout(() => inputRef.current?.focus(), 20)
  }, [open])
  const onQueryChange = (value: string) => {
    setQuery(value)
    setActive(0)
  }

  const select = (index: number) => {
    const item = filtered[index]
    if (!item) return
    void item.run()
    onOpenChange(false)
    setQuery('')
  }

  return (
    <Modal open={open} onOpenChange={onOpenChange} title={t('common.search')}>
      <div className="command-search">
        <Search size={18} />
        <input
          ref={inputRef}
          value={query}
          onChange={(event) => onQueryChange(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === 'ArrowDown') {
              event.preventDefault()
              setActive((value) => Math.min(value + 1, filtered.length - 1))
            }
            if (event.key === 'ArrowUp') {
              event.preventDefault()
              setActive((value) => Math.max(0, value - 1))
            }
            if (event.key === 'Enter') select(active)
          }}
          placeholder={t('shell.command')}
        />
      </div>
      <div className="command-list" role="listbox">
        {filtered.map((item, index) => (
          <button
            type="button"
            key={`${item.category ?? ''}-${item.label}`}
            className={index === active ? 'is-active' : ''}
            onMouseEnter={() => setActive(index)}
            onClick={() => select(index)}
            role="option"
            aria-selected={index === active}
          >
            <item.icon size={17} />
            <span>{item.label}</span>
            <span className="flex items-center justify-end gap-2">
              {item.category && <small>{item.category}</small>}
              {item.state && <Badge tone={item.stateTone}>{item.state}</Badge>}
            </span>
          </button>
        ))}
      </div>
    </Modal>
  )
}
