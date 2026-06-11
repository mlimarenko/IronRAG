import { useTranslation } from 'react-i18next';
import { Link } from 'react-router-dom';
import { Code2, Languages, MonitorCog, Moon, Sun, Tag } from 'lucide-react';

import { Button } from '@/shared/components/ui/button';
import { Label } from '@/shared/components/ui/label';
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/shared/components/ui/select';
import { useApp } from '@/shared/contexts/app-context';
import { usePreferences, type ThemePreference } from '@/shared/contexts/preferences-context';
import { AVAILABLE_LOCALES } from '@/shared/types';
import { BUILD_VERSION_LABEL } from '@/shared/lib/build-version';

const THEME_OPTIONS: { value: ThemePreference; icon: typeof Sun }[] = [
  { value: 'light', icon: Sun },
  { value: 'dark', icon: Moon },
  { value: 'system', icon: MonitorCog },
];

/**
 * `/admin/system` (NAV-08) — instance settings home. Absorbs the dissolved
 * Settings stub (ADM-13/RM-02: the lone locale dropdown), surfaces the default
 * theme + locale controls, the API explorer entry, and the build/version line.
 */
export default function AdminSystemPage() {
  const { t } = useTranslation();
  const { locale, setLocale } = useApp();
  const { theme, setTheme } = usePreferences();

  return (
    <div className="flex flex-1 min-h-0 flex-col overflow-auto p-6">
      <div className="mb-5">
        <h2 className="text-base font-bold tracking-tight">{t('admin.systemPage.title')}</h2>
        <p className="text-sm text-muted-foreground">{t('admin.systemPage.subtitle')}</p>
      </div>

      <div className="max-w-2xl space-y-4">
        {/* Locale */}
        <div className="workbench-surface p-4">
          <div className="mb-3 flex items-center gap-2">
            <Languages className="h-4 w-4 text-muted-foreground" />
            <Label className="text-sm font-bold">{t('admin.systemPage.languageTitle')}</Label>
          </div>
          <p className="mb-2 text-xs text-muted-foreground">{t('admin.languageDesc')}</p>
          <Select value={locale} onValueChange={(value) => setLocale(value)}>
            <SelectTrigger className="max-w-xs">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {AVAILABLE_LOCALES.map((l) => (
                <SelectItem key={l.code} value={l.code}>
                  {l.nativeLabel}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>

        {/* Theme */}
        <div className="workbench-surface p-4">
          <div className="mb-3 flex items-center gap-2">
            <Sun className="h-4 w-4 text-muted-foreground" />
            <Label className="text-sm font-bold">{t('admin.systemPage.themeTitle')}</Label>
          </div>
          <p className="mb-2 text-xs text-muted-foreground">{t('admin.systemPage.themeDesc')}</p>
          <div className="inline-flex gap-1 rounded-xl border bg-surface-sunken p-1">
            {THEME_OPTIONS.map((option) => {
              const Icon = option.icon;
              const active = theme === option.value;
              return (
                <button
                  key={option.value}
                  type="button"
                  onClick={() => setTheme(option.value)}
                  aria-pressed={active}
                  className={`flex items-center gap-1.5 rounded-lg px-3 py-1.5 text-xs font-semibold transition-colors ${
                    active
                      ? 'bg-primary text-primary-foreground shadow-sm'
                      : 'text-muted-foreground hover:bg-accent/50'
                  }`}
                >
                  <Icon className="h-3.5 w-3.5" />
                  {t(`admin.systemPage.theme.${option.value}`)}
                </button>
              );
            })}
          </div>
        </div>

        {/* API explorer + version */}
        <div className="workbench-surface p-4">
          <div className="mb-3 flex items-center gap-2">
            <Code2 className="h-4 w-4 text-muted-foreground" />
            <Label className="text-sm font-bold">{t('admin.systemPage.developerTitle')}</Label>
          </div>
          <p className="mb-3 text-xs text-muted-foreground">{t('admin.systemPage.developerDesc')}</p>
          <Button asChild size="sm" variant="outline">
            <Link to="/swagger">
              <Code2 className="mr-1.5 h-3.5 w-3.5" />
              {t('shell.apiDocs')}
            </Link>
          </Button>
        </div>

        <div className="flex items-center gap-2 px-1 text-xs text-muted-foreground">
          <Tag className="h-3.5 w-3.5" />
          <span className="font-medium">{BUILD_VERSION_LABEL}</span>
        </div>
      </div>
    </div>
  );
}
