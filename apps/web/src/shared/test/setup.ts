import '@testing-library/jest-dom'
import { afterAll, afterEach, beforeAll } from 'vitest'

type ReactActGlobal = typeof globalThis & {
  IS_REACT_ACT_ENVIRONMENT?: boolean
}
;(globalThis as ReactActGlobal).IS_REACT_ACT_ENVIRONMENT = true

const createTestStorage = (): Storage => {
  const items = new Map<string, string>()

  return {
    get length() {
      return items.size
    },
    clear: () => items.clear(),
    getItem: (key: string) => items.get(key) ?? null,
    key: (index: number) => Array.from(items.keys())[index] ?? null,
    removeItem: (key: string) => {
      items.delete(key)
    },
    setItem: (key: string, value: string) => {
      items.set(key, value)
    },
  }
}

const testStorage = createTestStorage()

Object.defineProperty(window, 'localStorage', {
  configurable: true,
  value: testStorage,
})

Object.defineProperty(globalThis, 'localStorage', {
  configurable: true,
  value: testStorage,
})

// Import MSW only after replacing Node's experimental Web Storage getter.
// MSW reads `globalThis.localStorage` during module initialization.
const { server } = await import('../api/mocks/server')

if (!Element.prototype.scrollTo) {
  Object.defineProperty(Element.prototype, 'scrollTo', {
    configurable: true,
    value: () => {},
  })
}

await import('../i18n')

// Sprint 5: MSW lifecycle. `bypass` for unhandled requests is critical so
// existing `vi.mock('@/shared/api')` and `vi.spyOn(Tag, 'method')` overrides keep
// winning — MSW only intercepts requests that actually hit the network.
beforeAll(() => server.listen({ onUnhandledRequest: 'bypass' }))
afterEach(() => server.resetHandlers())
afterAll(() => server.close())

Object.defineProperty(window, 'matchMedia', {
  writable: true,
  value: (query: string) => ({
    matches: false,
    media: query,
    onchange: null,
    addListener: () => {},
    removeListener: () => {},
    addEventListener: () => {},
    removeEventListener: () => {},
    dispatchEvent: () => {},
  }),
})
