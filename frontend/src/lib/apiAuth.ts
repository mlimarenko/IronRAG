const API_TOKEN_KEY = 'rustrag:api-bearer-token'

function hasSessionStorage(): boolean {
  return typeof window !== 'undefined' && typeof window.sessionStorage !== 'undefined'
}

export function getApiBearerToken(): string {
  if (!hasSessionStorage()) {
    return ''
  }

  return window.sessionStorage.getItem(API_TOKEN_KEY) ?? ''
}

export function setApiBearerToken(token: string): void {
  if (!hasSessionStorage()) {
    return
  }

  const normalized = token.trim()
  if (normalized) {
    window.sessionStorage.setItem(API_TOKEN_KEY, normalized)
  } else {
    window.sessionStorage.removeItem(API_TOKEN_KEY)
  }
}

export function clearApiBearerToken(): void {
  if (!hasSessionStorage()) {
    return
  }

  window.sessionStorage.removeItem(API_TOKEN_KEY)
}

export function maskApiBearerToken(token: string): string {
  const normalized = token.trim()
  if (normalized.length <= 12) {
    return normalized
  }

  return `${normalized.slice(0, 8)}…${normalized.slice(-4)}`
}
