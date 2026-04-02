import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

import { describe, expect, it } from 'vitest'

const graphPagePath = fileURLToPath(new URL('./GraphPage.vue', import.meta.url))

describe('GraphPage loading contract', () => {
  it('uses a dedicated loading shell instead of reusing the generic coverage card for initial loading', () => {
    const source = readFileSync(graphPagePath, 'utf8')

    expect(source).toContain(
      "import GraphLoadingState from 'src/components/graph/GraphLoadingState.vue'",
    )
    expect(source).toContain('v-if="overlayState.tone === \'loading\'"')
    expect(source).toContain('<GraphLoadingState')
    expect(source).toContain('<GraphCoverageStateCard')
  })
})
