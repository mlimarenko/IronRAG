import { useTranslation } from 'react-i18next';
import { Link } from 'react-router-dom';
import { Code2, Languages, MonitorCog, Moon, Sun, Terminal } from 'lucide-react';

import { Button } from '@/shared/components/ui/button';
import { Label } from '@/shared/components/ui/label';
import { PageHeader } from '@/shared/components/layout/PageHeader';
import { PageShell } from '@/shared/components/layout/PageShell';
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
import { McpConnectGuide } from './McpTab';

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
    <PageShell
      header={
        <PageHeader
          title={t('admin.nav.system')}
          description={t('admin.systemPage.subtitle')}
        />
      }
      bodyScroll="auto"
      bodyClassName="p-3 animate-fade-in sm:p-4"
    >
      <div className="w-full space-y-4">
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
          <div className="inline-flex gap-1 rounded-md border bg-background p-0.5">
            {THEME_OPTIONS.map((option) => {
              const Icon = option.icon;
              const active = theme === option.value;
              return (
                <button
                  key={option.value}
                  type="button"
                  onClick={() => setTheme(option.value)}
                  aria-pressed={active}
                  className={`flex items-center gap-1.5 rounded px-3 py-1.5 text-xs font-medium transition-colors ${
                    active
                      ? 'bg-muted text-foreground'
                      : 'text-muted-foreground hover:bg-muted/70 hover:text-foreground'
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

        {/* MCP connection — instance-level integration reference (same for every
            library), lifted out of the dissolved per-library hub. */}
        <div className="workbench-surface p-4">
          <div className="mb-1 flex items-center gap-2">
            <Terminal className="h-4 w-4 text-muted-foreground" />
            <Label className="text-sm font-bold">{t('admin.systemPage.mcpTitle')}</Label>
          </div>
          <p className="mb-3 text-xs text-muted-foreground">{t('admin.systemPage.mcpDesc')}</p>
          <McpConnectGuide t={t} />
        </div>

      </div>
    </PageShell>
  );
}
