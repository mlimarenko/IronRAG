/**
 * Redaction and normalisation for browser telemetry payloads.
 *
 * Telemetry may carry URLs, error messages and stack traces, so every value is
 * treated as untrusted: personal identifiers, credentials and URL query strings
 * never leave the browser.
 */
const MAX_SANITIZED_TEXT_LENGTH = 512
const MAX_ATTRIBUTE_LENGTH = 256

const EMAIL_PATTERN = /[\p{L}\p{N}._%+-]+@[\p{L}\p{N}.-]+\.[\p{L}]{2,}/gu
const CREDENTIAL_PATTERN = /\b(?:Bearer|Basic)\s+[A-Za-z0-9._~+/=-]{8,}/gi
const JWT_PATTERN = /\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\b/g
const SECRET_QUERY_PARAMETER_PATTERN =
  /([?&](?:access_token|refresh_token|id_token|token|api[_-]?key|key|secret|password|passwd|code|state)=)[^&#\s]*/gi
const URL_QUERY_PATTERN = /(\bhttps?:\/\/[^\s"'<>)]+?)[?#][^\s"'<>)]*/gi
const FILE_URL_PATTERN = /\bfile:\/\/\/[^\s"'<>)]+/gi
const WINDOWS_PATH_PATTERN = /\b[A-Za-z]:\\Users\\[^\s"'<>)]+/gi
// Long digit runs are phone-number shaped. Keep line:column pairs and hashes,
// which never reach seven consecutive digits in a stack frame.
const PHONE_PATTERN = /(?<!\p{N})(?:\+?\d[\s().-]?){6,}\d(?!\p{N})/gu
// eslint-disable-next-line no-control-regex -- control characters are exactly what this strips
const CONTROL_CHARACTER_PATTERN = /[\u0000-\u001f\u007f]+/g
const WHITESPACE_PATTERN = /\s+/g
const ATTRIBUTE_NAME_PATTERN = /^[a-z][a-z0-9._-]{0,63}$/
const FORBIDDEN_ATTRIBUTE_NAME_PATTERN =
  /authorization|cookie|email|login|password|passwd|phone|secret|token|user(name|_id)?$|query|content|document|prompt/i

export function redactText(input: unknown, limit = MAX_SANITIZED_TEXT_LENGTH): string {
  if (input === null || input === undefined) return ''
  const raw = typeof input === 'string' ? input : String(input)
  const redacted = raw
    .replace(CREDENTIAL_PATTERN, '[redacted-credential]')
    .replace(JWT_PATTERN, '[redacted-token]')
    .replace(SECRET_QUERY_PARAMETER_PATTERN, '$1[redacted]')
    .replace(URL_QUERY_PATTERN, '$1')
    .replace(EMAIL_PATTERN, '[redacted-email]')
    .replace(FILE_URL_PATTERN, '[local-file]')
    .replace(WINDOWS_PATH_PATTERN, '[local-file]')
    .replace(PHONE_PATTERN, '[redacted-phone]')
  const normalized = redacted
    .replace(CONTROL_CHARACTER_PATTERN, ' ')
    .replace(WHITESPACE_PATTERN, ' ')
    .trim()
  return normalized.length > limit ? `${normalized.slice(0, limit)}…` : normalized
}

/**
 * Turn a concrete path into a low-cardinality route suitable for metrics:
 * generated identifiers are replaced, query strings and fragments are dropped.
 */
export function normalizeRoute(input: string | null | undefined): string {
  if (!input) return 'unknown'
  const pathOnly = input.split('?')[0]?.split('#')[0] ?? 'unknown'
  const segments = pathOnly.split('/').filter(Boolean).slice(0, 8)
  const normalized = segments.map((segment) => {
    if (/^\d+$/.test(segment)) return ':id'
    if (/^[0-9a-f]{8,}$/i.test(segment)) return ':id'
    if (/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(segment)) {
      return ':id'
    }
    return segment
  })
  const route = `/${normalized.join('/')}`
  return route.length > 128 ? route.slice(0, 128) : route
}

export function sanitizeAttributeValue(value: unknown): string | number | boolean | undefined {
  if (typeof value === 'boolean') return value
  if (typeof value === 'number') return Number.isFinite(value) ? value : undefined
  if (typeof value === 'string') {
    const clean = redactText(value, MAX_ATTRIBUTE_LENGTH)
    return clean.length > 0 ? clean : undefined
  }
  return undefined
}

export function sanitizeAttributes(
  attributes: Record<string, unknown>,
): Record<string, string | number | boolean> {
  const result: Record<string, string | number | boolean> = {}
  for (const [name, value] of Object.entries(attributes)) {
    if (!ATTRIBUTE_NAME_PATTERN.test(name) || FORBIDDEN_ATTRIBUTE_NAME_PATTERN.test(name)) continue
    const clean = sanitizeAttributeValue(value)
    if (clean !== undefined) result[name] = clean
  }
  return result
}
