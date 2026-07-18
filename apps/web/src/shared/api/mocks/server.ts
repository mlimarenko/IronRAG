// MSW Node server for vitest. Lifecycle is wired in src/shared/test/setup.ts.
// Default unhandled-request policy is `bypass` so existing `vi.mock('@/shared/api')`
// and `vi.spyOn(Tag, 'method')` overrides keep winning — MSW only intercepts
// requests that actually hit the network layer.

import { setupServer } from 'msw/node'
import { handlers } from './handlers'

export const server = setupServer(...handlers)
