export type WireRecord = Record<string, unknown>

export function normalizeWireRecord(value: unknown): WireRecord {
  return value && typeof value === 'object' ? (value as WireRecord) : {}
}

export function readWireValue(record: WireRecord, ...keys: string[]): unknown {
  for (const key of keys) {
    if (key in record) {
      return record[key]
    }
  }
  return undefined
}

export function normalizeWireString(value: unknown, fallback = ''): string {
  if (typeof value === 'string') {
    return value
  }
  if (typeof value === 'number' || typeof value === 'boolean' || typeof value === 'bigint') {
    return String(value)
  }
  return fallback
}

export function normalizeWireNullableString(value: unknown): string | null {
  if (value === null || value === undefined || value === '') {
    return null
  }
  if (
    typeof value === 'string' ||
    typeof value === 'number' ||
    typeof value === 'boolean' ||
    typeof value === 'bigint'
  ) {
    return String(value)
  }
  return null
}

export function normalizeWireNumber(value: unknown, fallback = 0): number {
  if (typeof value === 'number' && Number.isFinite(value)) {
    return value
  }
  const parsed = Number(value)
  return Number.isFinite(parsed) ? parsed : fallback
}

export function normalizeWireNullableNumber(value: unknown): number | null {
  if (value === null || value === undefined || value === '') {
    return null
  }
  const parsed = Number(value)
  return Number.isFinite(parsed) ? parsed : null
}

export function normalizeWireStringArray(value: unknown): string[] {
  return Array.isArray(value) ? value.map((item) => normalizeWireString(item)).filter(Boolean) : []
}
