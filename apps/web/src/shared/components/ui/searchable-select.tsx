import { useMemo, useState, type ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import { Check, ChevronDown, Search } from 'lucide-react';

import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/shared/components/ui/dropdown-menu';
import { Input } from '@/shared/components/ui/input';
import { cn } from '@/shared/lib/utils';

export type SearchableSelectOption = {
  value: string;
  label: string;
  description?: string;
  disabled?: boolean;
  searchKeywords?: string;
};

type SearchableSelectProps = {
  value: string;
  options: SearchableSelectOption[];
  onValueChange: (value: string) => void;
  placeholder?: string;
  searchPlaceholder?: string;
  emptyMessage?: string;
  disabled?: boolean;
  triggerClassName?: string;
  renderTriggerLabel?: (option: SearchableSelectOption | null) => ReactNode;
};

export function SearchableSelect({
  value,
  options,
  onValueChange,
  placeholder,
  searchPlaceholder,
  emptyMessage,
  disabled,
  triggerClassName,
  renderTriggerLabel,
}: SearchableSelectProps) {
  const { t } = useTranslation();
  const [search, setSearch] = useState('');

  const selected = useMemo(
    () => options.find(option => option.value === value) ?? null,
    [options, value],
  );
  const filtered = useMemo(() => {
    const query = search.trim().toLocaleLowerCase();
    if (!query) return options;
    return options.filter(option => {
      const haystack = `${option.label} ${option.description ?? ''} ${option.searchKeywords ?? ''}`.toLocaleLowerCase();
      return haystack.includes(query);
    });
  }, [options, search]);

  return (
    <DropdownMenu onOpenChange={open => { if (!open) setSearch(''); }}>
      <DropdownMenuTrigger
        disabled={disabled}
        className={cn(
          'flex h-10 w-full items-center justify-between gap-2 rounded-md border border-input bg-background px-3 py-2 text-sm ring-offset-background transition-colors',
          'focus:outline-none focus:ring-2 focus:ring-ring focus:ring-offset-2',
          'disabled:cursor-not-allowed disabled:opacity-50',
          'data-[state=open]:ring-2 data-[state=open]:ring-ring',
          triggerClassName,
        )}
      >
        <span className={cn('truncate text-left', !selected && 'text-muted-foreground')}>
          {renderTriggerLabel
            ? renderTriggerLabel(selected)
            : selected?.label ?? placeholder ?? ''}
        </span>
        <ChevronDown className="h-4 w-4 shrink-0 opacity-50" />
      </DropdownMenuTrigger>
      <DropdownMenuContent
        align="start"
        className="w-[min(28rem,calc(100vw-2rem))] max-h-[min(32rem,calc(100vh-5rem))] overflow-hidden p-0"
      >
        <div className="border-b p-2">
          <div className="relative">
            <Search className="pointer-events-none absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
            <Input
              autoFocus
              value={search}
              onChange={event => setSearch(event.target.value)}
              onKeyDown={event => event.stopPropagation()}
              placeholder={searchPlaceholder ?? t('common.search')}
              className="h-8 pl-8 text-xs"
            />
          </div>
        </div>
        <div className="max-h-[min(22rem,calc(100vh-13rem))] overflow-y-auto p-1">
          {filtered.length === 0 ? (
            <div className="px-2 py-3 text-xs text-muted-foreground">
              {emptyMessage ?? t('common.noData')}
            </div>
          ) : (
            filtered.map(option => {
              const isSelected = option.value === value;
              return (
                <DropdownMenuItem
                  key={option.value}
                  disabled={option.disabled}
                  onClick={() => onValueChange(option.value)}
                  className="flex items-start gap-2"
                >
                  <Check
                    className={cn(
                      'mt-0.5 h-3.5 w-3.5 shrink-0 transition-opacity',
                      isSelected ? 'opacity-100 text-primary' : 'opacity-0',
                    )}
                  />
                  <div className="min-w-0 flex-1">
                    <div className="truncate text-sm font-medium">{option.label}</div>
                    {option.description && (
                      <div className="truncate text-[11px] text-muted-foreground">
                        {option.description}
                      </div>
                    )}
                  </div>
                </DropdownMenuItem>
              );
            })
          )}
        </div>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
