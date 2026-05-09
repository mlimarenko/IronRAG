import type { TFunction } from 'i18next';
import { Label } from '@/shared/components/ui/label';
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/shared/components/ui/select';
import { AVAILABLE_LOCALES } from '@/shared/types';
import type { Locale } from '@/shared/types';

type SettingsTabProps = {
  t: TFunction;
  locale: Locale;
  setLocale: (locale: Locale) => void;
};

export function SettingsTab({ t, locale, setLocale }: SettingsTabProps) {
  return (
    <>
      <h2 className="text-base font-bold tracking-tight mb-5">{t('admin.settings')}</h2>
      <div className="max-w-md space-y-6">
        <div>
          <Label className="text-sm font-semibold">{t('admin.language')}</Label>
          <p className="text-xs text-muted-foreground mt-1 mb-2">
            {t('admin.languageDesc')}
          </p>
          <Select value={locale} onValueChange={(v) => setLocale(v)}>
            <SelectTrigger className="mt-1">
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
      </div>
    </>
  );
}
