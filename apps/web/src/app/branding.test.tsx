import { readFileSync } from 'node:fs'
import { cleanup, render, screen } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { Brand, BrandMark } from './Brand'
import { getBranding, initializeBranding, parseBranding } from './branding'

const origin = 'https://knowledge.example.com'

function serveConfig(config: unknown) {
  return vi.stubGlobal('fetch', vi.fn().mockResolvedValue(Response.json(config)))
}

afterEach(() => {
  cleanup()
  vi.unstubAllGlobals()
  document.head
    .querySelectorAll('link[rel="icon"], meta[property="og:title"]')
    .forEach((node) => node.remove())
})

describe('deployment branding validation', () => {
  it('ships an empty runtime config which preserves IronRAG defaults', () => {
    const shipped: unknown = JSON.parse(readFileSync('public/branding.json', 'utf8'))
    expect(parseBranding(shipped, origin)).toEqual({
      displayName: 'IronRAG',
      logoUrl: '/favicon.svg',
      iconUrl: '/favicon.svg',
      faviconUrl: '/favicon.svg',
    })
  })

  it('uses a single custom logo for omitted compact icon and favicon', () => {
    const config = parseBranding(
      { displayName: '  База знаний  ', logoUrl: '/brand/logo.svg' },
      origin,
    )
    expect(config).toEqual({
      displayName: 'База знаний',
      logoUrl: '/brand/logo.svg',
      iconUrl: '/brand/logo.svg',
      faviconUrl: '/brand/logo.svg',
    })
    expect(Object.isFrozen(config)).toBe(true)
  })

  it('uses the compact icon as the favicon unless explicitly configured', () => {
    expect(parseBranding({ iconUrl: '/brand/icon.png' }, origin).faviconUrl).toBe('/brand/icon.png')
  })

  it('accepts same-origin absolute HTTP(S) URLs', () => {
    expect(parseBranding({ logoUrl: `${origin}/brand/logo.svg?revision=2` }, origin).logoUrl).toBe(
      `${origin}/brand/logo.svg?revision=2`,
    )
    expect(
      parseBranding({ logoUrl: 'http://localhost:19000/logo.png' }, 'http://localhost:19000')
        .logoUrl,
    ).toBe('http://localhost:19000/logo.png')
  })

  const unsafeUrls = [
    'javascript:alert(1)',
    'JaVaScRiPt:alert(1)',
    'data:image/svg+xml,<svg/>',
    'blob:https://knowledge.example.com/id',
    'file:///logo.png',
    'ftp://knowledge.example.com/logo.png',
    '//evil.example.com/logo.png',
    '//knowledge.example.com/logo.png',
    '/\\evil.example.com/logo.png',
    'https://evil.example.com/logo.png',
    'http://knowledge.example.com/logo.png',
    'https://knowledge.example.com:444/logo.png',
    'https://user:password@knowledge.example.com/logo.png',
    'https://knowledge.example.com@evil.example.com/logo.png',
    'https://[invalid/logo.png',
    '/brand/\nlogo.png',
    '/brand/\tlogo.png',
    '/brand/ logo.png',
    ' /brand/logo.png',
    'logo.png',
    '',
  ]
  it.each(unsafeUrls)('rejects unsafe asset URL %j in every branding field', (value) => {
    for (const field of ['logoUrl', 'iconUrl', 'faviconUrl']) {
      expect(() => parseBranding({ [field]: value }, origin)).toThrow()
    }
  })

  it.each([
    null,
    [],
    'IronRAG',
    { displayName: '' },
    { displayName: '   ' },
    { displayName: 'a'.repeat(121) },
    { displayName: 'Knowledge\nWorkbench' },
    { displayName: 123 },
    { logoUrl: null },
    { permissions: ['admin'] },
    { display_name: 'Another name' },
  ])('rejects malformed config and non-branding fields: %j', (value) => {
    expect(() => parseBranding(value, origin)).toThrow()
  })
})

describe('branding bootstrap and components', () => {
  it('bounds the startup request and propagates cancellation', async () => {
    const reason = new DOMException('Timed out', 'TimeoutError')
    const signal = AbortSignal.abort(reason)
    const timeout = vi.spyOn(AbortSignal, 'timeout').mockReturnValue(signal)
    vi.stubGlobal(
      'fetch',
      vi.fn((_url: string, options: RequestInit) => {
        expect(options.signal).toBe(signal)
        return Promise.reject(reason)
      }),
    )
    try {
      await expect(initializeBranding()).rejects.toHaveProperty('name', 'TimeoutError')
      expect(timeout).toHaveBeenCalledWith(5000)
    } finally {
      timeout.mockRestore()
    }
  })

  it('loads fresh runtime config before updating title, favicon, and all brand variants', async () => {
    serveConfig({
      displayName: 'Team Knowledge',
      logoUrl: '/brand/logo.svg',
      iconUrl: '/brand/icon.svg',
      faviconUrl: '/brand/favicon.png',
    })
    document.head.insertAdjacentHTML(
      'beforeend',
      '<meta property="og:title" content="IronRAG"><link rel="icon" type="image/svg+xml" href="/favicon.svg">',
    )
    await initializeBranding()
    expect(fetch).toHaveBeenCalledWith(
      '/branding.json',
      expect.objectContaining({
        cache: 'no-store',
        credentials: 'same-origin',
        headers: { Accept: 'application/json' },
        signal: expect.any(AbortSignal),
      }),
    )
    expect(getBranding().displayName).toBe('Team Knowledge')
    expect(Object.isFrozen(getBranding())).toBe(true)
    expect(document.title).toBe('Team Knowledge')
    expect(document.querySelector('meta[property="og:title"]')).toHaveAttribute(
      'content',
      'Team Knowledge',
    )
    expect(document.querySelector('link[rel="icon"]')).toHaveAttribute('href', '/brand/favicon.png')
    expect(document.querySelector('link[rel="icon"]')).not.toHaveAttribute('type')

    const { container } = render(
      <>
        <Brand />
        <Brand compact />
        <BrandMark size={40} className="login-mark" />
      </>,
    )
    expect(screen.getByText('Team Knowledge')).toBeInTheDocument()
    expect(screen.getAllByLabelText('Team Knowledge')).toHaveLength(2)
    const marks = container.querySelectorAll('img')
    expect(Array.from(marks, (mark) => mark.getAttribute('src'))).toEqual([
      '/brand/logo.svg',
      '/brand/icon.svg',
      '/brand/logo.svg',
    ])
    expect(screen.queryByRole('img')).not.toBeInTheDocument()
    expect(marks[2]).toHaveAttribute('width', '40')
    expect(marks[2]).toHaveClass('login-mark')
    expect(marks[2]).toHaveStyle({ width: '40px', height: '40px' })
  })

  it('creates the favicon if absent and renders deployment name as text, not markup', async () => {
    serveConfig({ displayName: '<img src=x onerror=alert(1)>' })
    await initializeBranding()
    const { container } = render(<Brand />)
    expect(screen.getByText('<img src=x onerror=alert(1)>')).toBeInTheDocument()
    expect(container.querySelectorAll('img')).toHaveLength(1)
    expect(document.querySelectorAll('link[rel="icon"]')).toHaveLength(1)
  })

  it('renders the default name and assets when the config is empty', async () => {
    serveConfig({})
    await initializeBranding()
    const { container } = render(<Brand />)
    expect(screen.getByText('IronRAG')).toBeInTheDocument()
    expect(container.querySelector('img')).toHaveAttribute('src', '/favicon.svg')
    expect(document.title).toBe('IronRAG')
  })

  it('keeps a long deployment name accessible when its visible label is truncated', async () => {
    const displayName = 'A'.repeat(120)
    serveConfig({ displayName })
    await initializeBranding()
    render(<Brand />)
    expect(screen.getByLabelText(displayName)).toBeInTheDocument()
    expect(screen.getByTitle(displayName)).toHaveTextContent(displayName)
    expect(screen.getByTitle(displayName)).toHaveStyle({
      minWidth: '0',
      overflow: 'hidden',
      textOverflow: 'ellipsis',
    })
  })

  it.each(['http', 'json', 'schema', 'network'])(
    'rejects %s failures without applying unvalidated branding',
    async (failure) => {
      serveConfig({})
      await initializeBranding()
      const fetchMock = vi.fn()
      if (failure === 'network') fetchMock.mockRejectedValue(new TypeError('Network failed'))
      else if (failure === 'http') fetchMock.mockResolvedValue(new Response('', { status: 404 }))
      else if (failure === 'json')
        fetchMock.mockResolvedValue(new Response('<html>not json</html>'))
      else
        fetchMock.mockResolvedValue(
          Response.json({ displayName: 'Unsafe', logoUrl: 'javascript:alert(1)' }),
        )
      vi.stubGlobal('fetch', fetchMock)
      await expect(initializeBranding()).rejects.toThrow()
      expect(document.title).toBe('IronRAG')
      expect(getBranding().displayName).toBe('IronRAG')
    },
  )
})
