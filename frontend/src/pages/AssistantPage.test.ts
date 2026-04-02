import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

import { describe, expect, it } from 'vitest'

const assistantPagePath = fileURLToPath(new URL('./AssistantPage.vue', import.meta.url))
const assistantVerificationPath = fileURLToPath(
  new URL('../components/assistant/AssistantVerificationBanner.vue', import.meta.url),
)

describe('AssistantPage layout contract', () => {
  it('keeps summary and trust signals in the right context rail instead of the top header', () => {
    const source = readFileSync(assistantPagePath, 'utf8')

    expect(source).toContain('class="rr-assistant-page__context-summary"')
    expect(source).toContain('class="rr-assistant-page__context-signals"')
    expect(source).not.toContain('v-if="showHeaderSummary"')
  })

  it('keeps verification and readiness out of the main chat column chrome', () => {
    const source = readFileSync(assistantPagePath, 'utf8')
    const contextSignalsIndex = source.indexOf('rr-assistant-page__context-signals')
    const evidenceIndex = source.indexOf('<AssistantEvidencePanel')
    const threadBodyIndex = source.indexOf('rr-assistant-chat__thread-body')

    expect(contextSignalsIndex).toBeGreaterThan(threadBodyIndex)
    expect(evidenceIndex).toBeGreaterThan(contextSignalsIndex)
  })

  it('supports pasting clipboard images into the composer through the canonical upload path', () => {
    const source = readFileSync(assistantPagePath, 'utf8')

    expect(source).toContain('function handleComposerPaste(event: ClipboardEvent): void')
    expect(source).toContain("transientNotice.value = t('assistant.notices.imagesPasted'")
    expect(source).toContain('@paste="handleComposerPaste"')
  })

  it('preserves the last readiness summary when background summary refresh fails', () => {
    const source = readFileSync(assistantPagePath, 'utf8')
    const syncSectionStart = source.indexOf('async function syncLibraryReadiness')
    const syncSectionEnd = source.indexOf('async function replaceSessionQuery', syncSectionStart)
    const syncSection = source.slice(syncSectionStart, syncSectionEnd)

    expect(syncSection).toContain(
      "transientNotice.value ??= t('assistant.notices.summaryUnavailable')",
    )
    expect(syncSection).not.toContain('libraryReadinessSummary.value = null')
    expect(syncSection).not.toContain('libraryGraphCoverage.value = null')
  })
})

describe('AssistantVerificationBanner contract', () => {
  it('aggregates warning pills by code and rewrites unsupported literal details into product language', () => {
    const source = readFileSync(assistantVerificationPath, 'utf8')

    expect(source).toContain('assistant.verification.codes.')
    expect(source).toContain('assistant.verification.messages.literalNotGrounded')
    expect(source).toContain('grouped.get(warning.code)')
    expect(source).toContain('rr-assistant-verification--compact')
  })
})
