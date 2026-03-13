export function getStatusTone(status?: string): 'positive' | 'warning' | 'negative' | 'neutral' {
  const normalized = status?.toLowerCase() ?? ''

  if (['grounded', 'complete', 'ok', 'success'].includes(normalized)) {
    return 'positive'
  }

  if (
    ['partial', 'weakly_grounded', 'weak', 'degraded', 'warning', 'fallback'].includes(
      normalized,
    )
  ) {
    return 'warning'
  }

  if (['failed', 'error', 'ungrounded', 'empty', 'blocked'].includes(normalized)) {
    return 'negative'
  }

  return 'neutral'
}

export function formatStatusLabel(status?: string): string {
  if (!status) {
    return 'Unknown'
  }

  return status
    .split(/[_-]/g)
    .filter(Boolean)
    .map((part) => part.charAt(0).toUpperCase() + part.slice(1))
    .join(' ')
}

export function formatDebugEntries(debugJson: Record<string, unknown>) {
  return Object.entries(debugJson).map(([key, value]) => ({
    key,
    preview: formatDebugValue(value),
  }))
}

function formatDebugValue(value: unknown): string {
  if (value == null) {
    return 'null'
  }

  if (typeof value === 'string') {
    return value
  }

  if (typeof value === 'number' || typeof value === 'boolean') {
    return String(value)
  }

  try {
    return JSON.stringify(value, null, 2)
  } catch {
    return '[unserializable value]'
  }
}
