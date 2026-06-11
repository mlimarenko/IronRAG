import { useMemo, useState, type ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import { ArrowDown, ArrowUp, ArrowUpDown, Search, X } from 'lucide-react';

import { DataState } from '@/shared/components/DataState';
import { Badge } from '@/shared/components/ui/badge';
import { Button } from '@/shared/components/ui/button';
import { Input } from '@/shared/components/ui/input';
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/shared/components/ui/select';
import {
  isStorageRecord,
  parseNumberOption,
  parseTableSort,
  useTableState,
  type TableSortState,
} from '@/shared/hooks/useTableState';
import type { AiConfigDataState } from '@/features/admin/model/aiConfig';

const PAGE_SIZE_OPTIONS = [25, 50, 100] as const;
const DEFAULT_PAGE_SIZE = 25;

export type EntityColumn<T> = {
  key: string;
  header: ReactNode;
  width?: string;
  align?: 'left' | 'right' | 'center';
  cell: (row: T) => ReactNode;
  sortValue?: (row: T) => string | number | null | undefined;
};

type EntityTableState = {
  pageSize: (typeof PAGE_SIZE_OPTIONS)[number];
  sort: TableSortState<string>;
};

const DEFAULT_TABLE_STATE: EntityTableState = {
  pageSize: DEFAULT_PAGE_SIZE,
  sort: null,
};

function getPageItems(current: number, total: number): Array<number | 'ellipsis'> {
  if (total <= 7) {
    return Array.from({ length: total }, (_, index) => index + 1);
  }
  const items: Array<number | 'ellipsis'> = [1];
  const start = Math.max(2, current - 1);
  const end = Math.min(total - 1, current + 1);
  if (start > 2) items.push('ellipsis');
  for (let page = start; page <= end; page += 1) items.push(page);
  if (end < total - 1) items.push('ellipsis');
  items.push(total);
  return items;
}

function compareSortValues(
  a: string | number | null | undefined,
  b: string | number | null | undefined,
): number {
  if (a == null && b == null) return 0;
  if (a == null) return 1;
  if (b == null) return -1;
  if (typeof a === 'number' && typeof b === 'number') {
    return a - b;
  }
  return String(a).localeCompare(String(b), undefined, {
    numeric: true,
    sensitivity: 'base',
  });
}

export type EntityInspector<T> = {
  title: ReactNode;
  subtitle?: ReactNode;
  body: ReactNode;
  actions?: ReactNode;
  row: T;
};

type EntityWorkbenchProps<T> = {
  tableId: string;
  title: string;
  count: number;
  state: AiConfigDataState<unknown>;
  rows: T[];
  rowKey: (row: T) => string;
  columns: EntityColumn<T>[];
  emptyMessage: string;
  matchesFilter?: (row: T, filter: string) => boolean;
  searchPlaceholder?: string;
  toolbar?: ReactNode;
  rowActions?: (row: T) => ReactNode;
  renderInspector?: (row: T) => EntityInspector<T> | null;
};

export function EntityWorkbench<T>({
  tableId,
  title,
  count,
  state,
  rows,
  rowKey,
  columns,
  emptyMessage,
  matchesFilter,
  searchPlaceholder,
  toolbar,
  rowActions,
  renderInspector,
}: EntityWorkbenchProps<T>) {
  const { t } = useTranslation();
  const [search, setSearch] = useState('');
  const [page, setPage] = useState(1);
  const [selectedKey, setSelectedKey] = useState<string | null>(null);
  const sortableColumnKeysList = useMemo(
    () => columns.filter(col => col.sortValue).map(col => col.key),
    [columns],
  );
  const [tableState, setTableState] = useTableState<EntityTableState>({
    tableId,
    defaultValue: DEFAULT_TABLE_STATE,
    parse: raw => {
      const record = isStorageRecord(raw) ? raw : {};
      return {
        pageSize: parseNumberOption(record.pageSize, PAGE_SIZE_OPTIONS, DEFAULT_PAGE_SIZE),
        sort: parseTableSort(record.sort, sortableColumnKeysList, DEFAULT_TABLE_STATE.sort),
      };
    },
  });
  const { pageSize, sort } = tableState;

  const sortableColumnKeys = useMemo(
    () => new Set(sortableColumnKeysList),
    [sortableColumnKeysList],
  );

  const toggleSort = (key: string) => {
    if (!sortableColumnKeys.has(key)) return;
    setTableState(prev => {
      const current = prev.sort;
      const nextSort =
        !current || current.key !== key
          ? { key, direction: 'asc' as const }
          : current.direction === 'asc'
            ? { key, direction: 'desc' as const }
            : null;
      return { ...prev, sort: nextSort };
    });
  };

  const filteredRows = useMemo(() => {
    const base = !matchesFilter || !search.trim()
      ? rows
      : rows.filter(row => matchesFilter(row, search));
    if (!sort) return base;
    const column = columns.find(col => col.key === sort.key);
    if (!column?.sortValue) return base;
    const sortValue = column.sortValue;
    const dir = sort.direction === 'asc' ? 1 : -1;
    return base
      .slice()
      .sort((left, right) => compareSortValues(sortValue(left), sortValue(right)) * dir);
  }, [columns, matchesFilter, rows, search, sort]);

  const totalPages = Math.max(1, Math.ceil(filteredRows.length / pageSize));
  const currentPage = Math.min(page, totalPages);
  const visibleStart = filteredRows.length === 0 ? 0 : (currentPage - 1) * pageSize + 1;
  const visibleEnd = Math.min(currentPage * pageSize, filteredRows.length);
  const pagedRows = useMemo(
    () => filteredRows.slice((currentPage - 1) * pageSize, currentPage * pageSize),
    [filteredRows, currentPage, pageSize],
  );

  // Both prior pagination effects (clamp page to currentPage when row
  // count shrinks; reset page to 1 on search/pageSize change) violated
  // the canonical React 19 hook contract (set-state-in-effect). Page is
  // now derived from `currentPage` directly, so we already render the
  // clamped value without needing a synchronizing effect; resets on
  // search / page-size change live in their respective setter handlers.

  const selectedRow = useMemo(() => {
    if (!selectedKey) return null;
    return rows.find(row => rowKey(row) === selectedKey) ?? null;
  }, [rows, rowKey, selectedKey]);
  const inspector = selectedRow && renderInspector ? renderInspector(selectedRow) : null;

  return (
    <div className="workbench-surface flex h-full min-h-[420px] flex-col overflow-hidden">
      <div className="flex flex-wrap items-center gap-3 border-b border-border/70 px-4 py-3">
        <div className="flex items-center gap-2">
          <h3 className="text-sm font-bold tracking-tight">{title}</h3>
          <Badge variant="outline" className="font-mono text-[11px]">{count}</Badge>
        </div>
        {matchesFilter && (
          <div className="relative ml-auto w-full max-w-[280px] sm:ml-2">
            <Search className="pointer-events-none absolute left-3 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
            <Input
              className="h-9 pl-9"
              value={search}
              onChange={event => {
                setSearch(event.target.value);
                setPage(1);
              }}
              placeholder={searchPlaceholder}
            />
          </div>
        )}
        {toolbar && <div className={`flex items-center gap-2 ${matchesFilter ? '' : 'ml-auto'}`}>{toolbar}</div>}
      </div>

      <div className="flex min-h-0 flex-1">
        <div className="flex min-h-0 flex-1 flex-col">
          <DataState
            query={state}
            emptyRender={(
              <div className="m-4 rounded-md border border-dashed border-border/70 p-6 text-sm text-muted-foreground">
                {emptyMessage}
              </div>
            )}
          >
            {() => (
              <div className="flex min-h-0 flex-1 flex-col">
                <div className="min-h-0 flex-1 overflow-auto">
                  {filteredRows.length === 0 ? (
                    <div className="m-4 rounded-md border border-dashed border-border/70 p-6 text-sm text-muted-foreground">
                      {emptyMessage}
                    </div>
                  ) : (
                    <table className="min-w-full table-auto text-sm">
                      <thead
                        className="sticky top-0 z-10"
                        style={{
                          background:
                            'linear-gradient(180deg, hsl(var(--card)), hsl(var(--card) / 0.95))',
                          backdropFilter: 'blur(8px)',
                        }}
                      >
                        <tr className="border-b text-left">
                          {columns.map(col => {
                            const sortable = Boolean(col.sortValue);
                            const active = sort?.key === col.key;
                            const Icon = active
                              ? sort?.direction === 'asc'
                                ? ArrowUp
                                : ArrowDown
                              : ArrowUpDown;
                            return (
                              <th
                                key={col.key}
                                className={`section-label whitespace-nowrap px-4 py-2.5 ${col.width ?? ''} ${col.align === 'right' ? 'text-right' : ''}`}
                              >
                                {sortable ? (
                                  <button
                                    type="button"
                                    onClick={() => toggleSort(col.key)}
                                    className={`inline-flex items-center gap-1 transition-colors hover:text-foreground ${
                                      col.align === 'right' ? 'flex-row-reverse' : ''
                                    } ${active ? 'text-foreground' : ''}`}
                                  >
                                    <span>{col.header}</span>
                                    <Icon
                                      className={`h-3 w-3 ${
                                        active ? 'text-foreground' : 'text-muted-foreground/50'
                                      }`}
                                    />
                                  </button>
                                ) : (
                                  col.header
                                )}
                              </th>
                            );
                          })}
                          {rowActions && (
                            <th className="section-label w-24 whitespace-nowrap px-4 py-2.5 text-right">
                              {t('admin.actions')}
                            </th>
                          )}
                        </tr>
                      </thead>
                      <tbody>
                        {pagedRows.map(row => {
                          const key = rowKey(row);
                          const selected = selectedKey === key;
                          return (
                            <tr
                              key={key}
                              className={`cursor-pointer border-b border-border/40 transition-colors ${
                                selected
                                  ? 'border-l-2 border-l-primary bg-primary/5'
                                  : 'hover:bg-accent/30'
                              }`}
                              onClick={() => setSelectedKey(selected ? null : key)}
                            >
                              {columns.map(col => (
                                <td
                                  key={col.key}
                                  className={`px-4 py-2.5 align-middle ${col.align === 'right' ? 'text-right' : ''}`}
                                >
                                  {col.cell(row)}
                                </td>
                              ))}
                              {rowActions && (
                                <td
                                  className="px-4 py-2.5 text-right align-middle"
                                  onClick={event => event.stopPropagation()}
                                >
                                  <div className="inline-flex items-center gap-1">
                                    {rowActions(row)}
                                  </div>
                                </td>
                              )}
                            </tr>
                          );
                        })}
                      </tbody>
                    </table>
                  )}
                </div>
                {filteredRows.length > 0 && (
                  <div className="shrink-0 border-t border-border/70 bg-background/95 px-4 py-2 backdrop-blur supports-[backdrop-filter]:bg-background/85">
                    <div className="flex flex-wrap items-center gap-3">
                      <span className="text-xs font-medium text-muted-foreground tabular-nums">
                        {t('documents.paginationSummary', {
                          from: visibleStart,
                          to: visibleEnd,
                          total: filteredRows.length,
                        })}
                      </span>
                      <div className="flex items-center gap-2 md:ml-auto">
                        <span className="text-xs text-muted-foreground">
                          {t('documents.pageSize')}
                        </span>
                        <Select
                          value={String(pageSize)}
                          onValueChange={value => {
                            setTableState(prev => ({
                              ...prev,
                              pageSize: Number(value) as EntityTableState['pageSize'],
                            }));
                            setPage(1);
                          }}
                        >
                          <SelectTrigger className="h-8 w-[80px] text-xs">
                            <SelectValue />
                          </SelectTrigger>
                          <SelectContent>
                            {PAGE_SIZE_OPTIONS.map(option => (
                              <SelectItem key={option} value={String(option)}>
                                {option}
                              </SelectItem>
                            ))}
                          </SelectContent>
                        </Select>
                      </div>
                      <div className="flex items-center gap-1">
                        <Button
                          variant="outline"
                          size="sm"
                          className="h-8 text-xs"
                          disabled={currentPage <= 1}
                          onClick={() => setPage(p => Math.max(1, p - 1))}
                        >
                          {t('documents.previous')}
                        </Button>
                        {getPageItems(currentPage, totalPages).map((item, index) =>
                          item === 'ellipsis' ? (
                            <span
                              key={`ellipsis-${index}`}
                              className="px-1.5 text-xs text-muted-foreground"
                            >
                              …
                            </span>
                          ) : (
                            <Button
                              key={item}
                              variant={item === currentPage ? 'default' : 'outline'}
                              size="sm"
                              className="h-8 min-w-8 px-2 text-xs tabular-nums"
                              aria-current={item === currentPage ? 'page' : undefined}
                              onClick={() => setPage(item)}
                            >
                              {item}
                            </Button>
                          ),
                        )}
                        <Button
                          variant="outline"
                          size="sm"
                          className="h-8 text-xs"
                          disabled={currentPage >= totalPages}
                          onClick={() => setPage(p => Math.min(totalPages, p + 1))}
                        >
                          {t('documents.next')}
                        </Button>
                      </div>
                    </div>
                  </div>
                )}
              </div>
            )}
          </DataState>
        </div>

        {inspector && (
          <aside className="hidden w-80 shrink-0 animate-slide-in-right overflow-y-auto border-l border-border/70 bg-card md:block lg:w-96">
            <div className="flex items-start justify-between gap-3 border-b border-border/70 p-4">
              <div className="min-w-0 flex-1">
                <h4 className="text-sm font-bold tracking-tight leading-5 [overflow-wrap:anywhere]">
                  {inspector.title}
                </h4>
                {inspector.subtitle && (
                  <div className="mt-1 text-xs text-muted-foreground [overflow-wrap:anywhere]">
                    {inspector.subtitle}
                  </div>
                )}
              </div>
              <button
                type="button"
                onClick={() => setSelectedKey(null)}
                className="shrink-0 rounded-lg p-1.5 transition-colors hover:bg-muted"
                aria-label={t('common.close')}
              >
                <X className="h-4 w-4" />
              </button>
            </div>
            <div className="space-y-4 p-4">{inspector.body}</div>
            {inspector.actions && (
              <div className="space-y-2 border-t border-border/70 p-4">{inspector.actions}</div>
            )}
          </aside>
        )}
      </div>
    </div>
  );
}

export function InspectorField({
  label,
  value,
  mono = false,
}: {
  label: ReactNode;
  value: ReactNode;
  mono?: boolean;
}) {
  return (
    <div className="flex justify-between gap-3 text-sm">
      <span className="text-muted-foreground">{label}</span>
      <span
        className={`text-right font-semibold [overflow-wrap:anywhere] ${mono ? 'font-mono text-xs' : ''}`}
      >
        {value}
      </span>
    </div>
  );
}

export function InspectorSection({ title, children }: { title: ReactNode; children: ReactNode }) {
  return (
    <div className="space-y-2">
      <div className="section-label">{title}</div>
      {children}
    </div>
  );
}
