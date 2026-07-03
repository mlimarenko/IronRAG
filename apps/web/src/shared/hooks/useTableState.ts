import { useMemo, type Dispatch, type SetStateAction } from "react";

import { useLocalStorageState } from "./useLocalStorageState";

const TABLE_STATE_STORAGE_PREFIX = "ironrag_table_state";

export type TableSortDirection = "asc" | "desc";

export type TableSortState<SortKey extends string> = {
  key: SortKey;
  direction: TableSortDirection;
} | null;

type UseTableStateOptions<T> = {
  tableId: string;
  defaultValue: T;
  parse: (raw: unknown) => T;
};

export function getTableStateStorageKey(tableId: string): string {
  return `${TABLE_STATE_STORAGE_PREFIX}:${tableId}`;
}

export function isStorageRecord(raw: unknown): raw is Record<string, unknown> {
  return typeof raw === "object" && raw !== null && !Array.isArray(raw);
}

export function parseNumberOption<Option extends number>(
  raw: unknown,
  options: readonly Option[],
  fallback: Option,
): Option {
  return typeof raw === "number" && options.some((option) => option === raw)
    ? (raw as Option)
    : fallback;
}

export function parseStringOption<Option extends string>(
  raw: unknown,
  options: readonly Option[],
  fallback: Option,
): Option {
  return typeof raw === "string" && options.some((option) => option === raw)
    ? (raw as Option)
    : fallback;
}

export function parseTableSort<SortKey extends string>(
  raw: unknown,
  sortKeys: readonly SortKey[],
  fallback: TableSortState<SortKey>,
): TableSortState<SortKey> {
  if (!isStorageRecord(raw)) return fallback;
  const key = raw.key;
  const direction = raw.direction;
  if (
    typeof key === "string" &&
    sortKeys.some((sortKey) => sortKey === key) &&
    (direction === "asc" || direction === "desc")
  ) {
    return { key: key as SortKey, direction };
  }
  return fallback;
}

export function useTableState<T>({
  tableId,
  defaultValue,
  parse,
}: UseTableStateOptions<T>): [T, Dispatch<SetStateAction<T>>] {
  const storageKey = useMemo(() => getTableStateStorageKey(tableId), [tableId]);
  return useLocalStorageState({ key: storageKey, defaultValue, parse });
}
