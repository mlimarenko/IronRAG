import { renderToStaticMarkup } from 'react-dom/server'
import type { ReactNode } from 'react'
import { waitFor } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

const { rootRender, i18nLoaded, appLoaded, installInterceptor } = vi.hoisted(() => ({
  rootRender: vi.fn<(node: ReactNode) => void>(),
  i18nLoaded: vi.fn<(displayName: string) => void>(),
  appLoaded: vi.fn(),
  installInterceptor: vi.fn(),
}))

vi.mock('react-dom/client', () => ({ createRoot: () => ({ render: rootRender }) }))
vi.mock('@/shared/api/sessionLifecycle', () => ({
  installSessionExpiryInterceptor: installInterceptor,
}))
vi.mock('./App.tsx', () => {
  appLoaded()
  return { default: () => <div>Application loaded</div> }
})
vi.mock('@/shared/i18n', async () => {
  const { getBranding } = await import('./branding')
  i18nLoaded(getBranding().displayName)
  return { default: { t: (key: string) => key } }
})

beforeEach(() => {
  vi.resetModules()
  vi.clearAllMocks()
  vi.stubEnv('VITE_ENABLE_MOCKS', 'false')
  vi.stubGlobal('requestIdleCallback', vi.fn())
  document.body.innerHTML = '<div id="root"></div>'
})

afterEach(() => {
  vi.unstubAllGlobals()
  vi.unstubAllEnvs()
  vi.restoreAllMocks()
})

describe('entrypoint branding startup order', () => {
  it('initializes deployment branding before evaluating i18n and App or rendering', async () => {
    let completeFetch!: (response: Response) => void
    vi.stubGlobal(
      'fetch',
      vi.fn(
        () =>
          new Promise<Response>((resolve) => {
            completeFetch = resolve
          }),
      ),
    )
    const startup = import('./main')
    await waitFor(() => expect(fetch).toHaveBeenCalledOnce())
    expect(rootRender).not.toHaveBeenCalled()
    expect(i18nLoaded).not.toHaveBeenCalled()
    expect(appLoaded).not.toHaveBeenCalled()
    expect(installInterceptor).toHaveBeenCalledOnce()

    completeFetch(Response.json({ displayName: 'Deployment Knowledge' }))
    await startup
    expect(i18nLoaded).toHaveBeenCalledWith('Deployment Knowledge')
    expect(rootRender).toHaveBeenCalledOnce()
    expect(renderToStaticMarkup(rootRender.mock.calls[0]?.[0])).toContain('Application loaded')
    expect(document.title).toBe('Deployment Knowledge')
  })

  it('blocks App on invalid config, shows localized error UI, and does not log raw input', async () => {
    vi.stubGlobal(
      'fetch',
      vi
        .fn()
        .mockResolvedValue(
          Response.json({ displayName: 'Rejected', logoUrl: 'javascript:alert(1)' }),
        ),
    )
    const error = vi.spyOn(console, 'error').mockImplementation(() => {})
    await import('./main')
    expect(appLoaded).not.toHaveBeenCalled()
    expect(rootRender).toHaveBeenCalledOnce()
    const markup = renderToStaticMarkup(rootRender.mock.calls[0]?.[0])
    expect(markup).toContain('common.rootErrorTitle')
    expect(markup).toContain('common.rootErrorDescription')
    expect(error).toHaveBeenCalledExactlyOnceWith(
      'Frontend branding configuration failed. Check /branding.json and reload.',
    )
  })
})
