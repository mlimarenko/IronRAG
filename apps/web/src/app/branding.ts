import { z } from 'zod'

export type Branding = Readonly<{
  displayName: string
  logoUrl: string
  iconUrl: string
  faviconUrl: string
}>

const defaults: Branding = Object.freeze({
  displayName: 'IronRAG',
  logoUrl: '/favicon.svg',
  iconUrl: '/favicon.svg',
  faviconUrl: '/favicon.svg',
})

// Loaded once before rendering. Branding is deployment presentation, never IAM state.
let branding: Branding = defaults

export function getBranding(): Branding {
  return branding
}

function hasControlCharacters(value: string) {
  return Array.from(value).some((character) => {
    const codePoint = character.codePointAt(0)
    return codePoint !== undefined && (codePoint < 32 || codePoint === 127)
  })
}

function isAssetUrl(value: string, origin: string) {
  if (
    hasControlCharacters(value) ||
    value.includes('\\') ||
    value.includes(' ') ||
    value.startsWith('//') ||
    !(value.startsWith('/') || /^https?:\/\//i.test(value))
  ) {
    return false
  }
  try {
    const url = new URL(value, origin)
    return (
      (url.protocol === 'http:' || url.protocol === 'https:') &&
      url.origin === origin &&
      !url.username &&
      !url.password
    )
  } catch {
    // Malformed URLs are invalid configuration, not a reason to guess another asset.
    return false
  }
}

export function parseBranding(input: unknown, origin: string): Branding {
  const assetUrl = z
    .string()
    .min(1)
    .max(2048)
    .refine((value) => isAssetUrl(value, origin), {
      message: 'Expected a same-origin HTTP(S) URL or root-relative asset path',
    })
  const config = z
    .strictObject({
      displayName: z
        .string()
        .trim()
        .min(1)
        .max(120)
        .refine((value) => !hasControlCharacters(value))
        .optional(),
      logoUrl: assetUrl.optional(),
      iconUrl: assetUrl.optional(),
      faviconUrl: assetUrl.optional(),
    })
    .parse(input)
  const logoUrl = config.logoUrl ?? defaults.logoUrl
  const iconUrl = config.iconUrl ?? logoUrl
  return Object.freeze({
    displayName: config.displayName ?? defaults.displayName,
    logoUrl,
    iconUrl,
    faviconUrl: config.faviconUrl ?? iconUrl,
  })
}

export async function initializeBranding() {
  const response = await fetch('/branding.json', {
    signal: AbortSignal.timeout(5000),
    cache: 'no-store',
    credentials: 'same-origin',
    headers: { Accept: 'application/json' },
  })
  if (!response.ok) throw new Error('Unable to load /branding.json')
  const config = parseBranding(await response.json(), window.location.origin)
  branding = config
  document.title = config.displayName
  document.querySelector('meta[property="og:title"]')?.setAttribute('content', config.displayName)
  const favicon =
    document.querySelector<HTMLLinkElement>('link[rel="icon"]') ?? document.createElement('link')
  favicon.rel = 'icon'
  // Do not retain the default SVG MIME hint for a custom PNG/ICO.
  favicon.removeAttribute('type')
  favicon.href = config.faviconUrl
  if (!favicon.isConnected) document.head.append(favicon)
}
