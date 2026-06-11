import { useTranslation } from 'react-i18next';
import { Check, ChevronDown, Globe, LogOut, Monitor, Moon, Sun } from 'lucide-react';

import { useApp } from '@/shared/contexts/app-context';
import { usePreferences } from '@/shared/contexts/preferences-context';
import { useCan } from '@/shared/auth/useCan';
import { AVAILABLE_LOCALES } from '@/shared/types';
import { Avatar } from '@/shared/components/ui/avatar';
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
  DropdownMenuTrigger,
} from '@/shared/components/ui/dropdown-menu';

/**
 * Global-preferences home: identity + role, theme, language, logout. Rendered
 * as a dropdown on desktop and inline (variant="inline") at
 * the bottom of the mobile drawer so logout is reachable on every viewport.
 */
export function UserMenu({
  variant = 'menu',
  onAfterAction,
}: {
  variant?: 'menu' | 'inline';
  onAfterAction?: () => void;
}) {
  const { t } = useTranslation();
  const { user, logout, locale, setLocale } = useApp();
  const { theme, setTheme } = usePreferences();
  const { role } = useCan();

  const name = user?.displayName ?? t('shell.userFallback');
  // Literal t() per role so the i18n static-analysis audit sees each key used.
  const roleLabel =
    role === 'admin'
      ? t('shell.roleAdmin')
      : role === 'operator'
        ? t('shell.roleOperator')
        : t('shell.roleViewer');

  const handleLogout = () => {
    void logout();
    onAfterAction?.();
  };

  // Labels resolved as literal t() calls (not via a key indirection) so the
  // i18n audit's AST tracer registers each theme key as used.
  const themeOptions = [
    { value: 'light' as const, icon: Sun, label: t('shell.themeLight') },
    { value: 'dark' as const, icon: Moon, label: t('shell.themeDark') },
    { value: 'system' as const, icon: Monitor, label: t('shell.themeSystem') },
  ];

  if (variant === 'inline') {
    return (
      <div className="space-y-3">
        <div className="flex items-center gap-3">
          <Avatar name={name} size="md" />
          <div className="min-w-0">
            <div className="truncate text-sm font-semibold text-shell-foreground">{name}</div>
            <div className="text-2xs font-medium uppercase tracking-wide text-shell-muted">
              {roleLabel}
            </div>
          </div>
        </div>

        <div>
          <div className="mb-1.5 text-2xs font-semibold uppercase tracking-wide text-shell-muted">
            {t('shell.theme')}
          </div>
          <div className="grid grid-cols-3 gap-1.5">
            {themeOptions.map((option) => (
              <button
                key={option.value}
                type="button"
                onClick={() => setTheme(option.value)}
                aria-pressed={theme === option.value}
                className={`flex items-center justify-center gap-1.5 rounded-md border px-2 py-1.5 text-xs font-medium transition-colors ${
                  theme === option.value
                    ? 'border-shell-active/40 bg-shell-active/15 text-shell-foreground'
                    : 'border-shell-border text-shell-muted hover:bg-shell-hover'
                }`}
              >
                <option.icon className="h-3.5 w-3.5" />
                {option.label}
              </button>
            ))}
          </div>
        </div>

        <div>
          <div className="mb-1.5 text-2xs font-semibold uppercase tracking-wide text-shell-muted">
            {t('shell.language')}
          </div>
          <div className="grid grid-cols-2 gap-1.5">
            {AVAILABLE_LOCALES.map((option) => (
              <button
                key={option.code}
                type="button"
                onClick={() => setLocale(option.code)}
                aria-pressed={locale === option.code}
                className={`flex items-center justify-center gap-1.5 rounded-md border px-2 py-1.5 text-xs font-medium transition-colors ${
                  locale === option.code
                    ? 'border-shell-active/40 bg-shell-active/15 text-shell-foreground'
                    : 'border-shell-border text-shell-muted hover:bg-shell-hover'
                }`}
              >
                {option.nativeLabel}
              </button>
            ))}
          </div>
        </div>

        <button
          type="button"
          onClick={handleLogout}
          className="flex w-full items-center gap-2 rounded-md border border-shell-border px-2.5 py-2 text-xs font-semibold text-shell-foreground transition-colors hover:bg-shell-hover"
        >
          <LogOut className="h-3.5 w-3.5" />
          {t('shell.logout')}
        </button>
      </div>
    );
  }

  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <button
          type="button"
          className="flex items-center gap-2 rounded-lg border border-shell-border bg-shell-hover px-2 py-1.5 text-xs text-shell-foreground outline-none transition-colors hover:bg-shell-active/15 focus-visible:ring-2 focus-visible:ring-shell-active/60"
          aria-label={t('shell.userMenu')}
        >
          <Avatar name={name} size="sm" />
          <span className="hidden max-w-[100px] truncate font-medium lg:inline">{name}</span>
          <ChevronDown className="h-3 w-3 opacity-50" />
        </button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="w-60">
        <div className="flex items-center gap-2.5 px-2 py-1.5">
          <Avatar name={name} size="md" />
          <div className="min-w-0">
            <div className="truncate text-sm font-semibold leading-tight">{name}</div>
            <div className="text-2xs font-medium uppercase tracking-wide text-muted-foreground">
              {roleLabel}
            </div>
          </div>
        </div>
        <DropdownMenuSeparator />

        <DropdownMenuSub>
          <DropdownMenuSubTrigger>
            <Sun className="mr-2 h-3.5 w-3.5" />
            {t('shell.theme')}
          </DropdownMenuSubTrigger>
          <DropdownMenuSubContent>
            {themeOptions.map((option) => (
              <DropdownMenuItem key={option.value} onClick={() => setTheme(option.value)}>
                <option.icon className="mr-2 h-3.5 w-3.5" />
                <span className="flex-1">{option.label}</span>
                {theme === option.value && <Check className="h-3.5 w-3.5" />}
              </DropdownMenuItem>
            ))}
          </DropdownMenuSubContent>
        </DropdownMenuSub>

        <DropdownMenuSub>
          <DropdownMenuSubTrigger>
            <Globe className="mr-2 h-3.5 w-3.5" />
            {t('shell.language')}
          </DropdownMenuSubTrigger>
          <DropdownMenuSubContent>
            {AVAILABLE_LOCALES.map((option) => (
              <DropdownMenuItem key={option.code} onClick={() => setLocale(option.code)}>
                <span className="flex-1">{option.nativeLabel}</span>
                {locale === option.code && <Check className="h-3.5 w-3.5" />}
              </DropdownMenuItem>
            ))}
          </DropdownMenuSubContent>
        </DropdownMenuSub>

        <DropdownMenuSeparator />
        <DropdownMenuLabel className="sr-only">{t('shell.account')}</DropdownMenuLabel>
        <DropdownMenuItem onClick={handleLogout}>
          <LogOut className="mr-2 h-3.5 w-3.5" />
          {t('shell.logout')}
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
