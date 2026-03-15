import { i18n } from 'src/boot/i18n'

function humanizeStatus(value: string): string {
  return value
    .replace(/[_-]+/g, ' ')
    .replace(/\b\w/g, (char) => char.toUpperCase())
}

function normalizeStatusKey(value?: string | null): string {
  return value?.trim().toLowerCase().replace(/[\s-]+/g, '_') ?? ''
}

export function translateStatusLabel(value?: string | null): string {
  const key = normalizeStatusKey(value)
  if (!key) {
    return i18n.global.t('common.status.unknown')
  }

  const path = `common.status.${key}`
  if (i18n.global.te(path)) {
    return i18n.global.t(path)
  }

  return humanizeStatus(value ?? '')
}

export function formatDebugValue(value: unknown): string {
  if (value == null) {
    return i18n.global.t('common.debug.nullValue')
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
    return i18n.global.t('common.debug.unserializableValue')
  }
}
