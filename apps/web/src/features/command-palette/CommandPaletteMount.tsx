import { useEffect, useState } from 'react';

import { onShellIntent } from '@/shared/lib/shell-events';
import { CommandPalette } from './CommandPalette';

/**
 * Self-contained mount for the global command palette. Owns the open state and
 * wires the two ways to summon it:
 *   - ⌘K (macOS) / Ctrl-K (everywhere else), captured at the document level.
 *   - the `open-command-palette` shell intent, so any surface can open it.
 *
 * Keeping all of this here lets the AppShell add the palette with a single
 * `<CommandPaletteMount />` element and no extra state — the surgical mount the
 * brief asks for.
 */
export function CommandPaletteMount() {
  const [open, setOpen] = useState(false);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if ((event.metaKey || event.ctrlKey) && (event.key === 'k' || event.key === 'K')) {
        event.preventDefault();
        setOpen((prev) => !prev);
      }
    };
    document.addEventListener('keydown', onKeyDown);
    return () => document.removeEventListener('keydown', onKeyDown);
  }, []);

  useEffect(() => onShellIntent('open-command-palette', () => setOpen(true)), []);

  return <CommandPalette open={open} onOpenChange={setOpen} />;
}
