<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import type { QueryVerificationState, QueryVerificationWarning } from 'src/services/api/query'

const props = withDefaults(
  defineProps<{
    state: QueryVerificationState
    warnings: QueryVerificationWarning[]
    compact?: boolean
  }>(),
  {
    compact: false,
  },
)

const { t } = useI18n()

interface VerificationWarningGroup {
  key: string
  code: string
  message: string
  count: number
}

interface VerificationWarningSummaryItem {
  key: string
  label: string
  count: number
}

const unsupportedCapability = computed(() =>
  props.warnings.some((warning) => warning.code.startsWith('unsupported')),
)

const tone = computed(() => {
  switch (props.state) {
    case 'verified':
      return 'success'
    case 'partially_supported':
      return 'warning'
    case 'conflicting_evidence':
    case 'insufficient_evidence':
      return 'warning'
    case 'failed':
      return 'error'
    default:
      return 'info'
  }
})

const title = computed(() => {
  if (unsupportedCapability.value) {
    return t('assistant.verification.unsupportedTitle')
  }
  return t(`assistant.verification.states.${props.state}.title`)
})

const body = computed(() => {
  if (unsupportedCapability.value) {
    return t('assistant.verification.unsupportedBody')
  }
  return t(`assistant.verification.states.${props.state}.body`)
})

function formatWarningCode(code: string): string {
  const translated = t(`assistant.verification.codes.${code}`)
  if (translated !== `assistant.verification.codes.${code}`) {
    return translated
  }
  return code.replace(/_/g, ' ').trim().toUpperCase()
}

function formatWarningMessage(warning: { code: string; message: string }): string {
  const unsupportedLiteralMatch = warning.message.match(
    /^Literal `(.+)` is not grounded in selected evidence\.$/i,
  )
  if (warning.code === 'unsupported_literal' && unsupportedLiteralMatch?.[1]) {
    return t('assistant.verification.messages.literalNotGrounded', {
      literal: unsupportedLiteralMatch[1],
    })
  }
  return warning.message
}

const warningGroups = computed<VerificationWarningGroup[]>(() => {
  const grouped = new Map<string, VerificationWarningGroup>()
  for (const warning of props.warnings) {
    const key = `${warning.code}:${warning.message}`
    const current = grouped.get(key)
    if (current) {
      current.count += 1
      continue
    }
    grouped.set(key, {
      key,
      code: warning.code,
      message: warning.message,
      count: 1,
    })
  }
  return Array.from(grouped.values()).sort((left, right) =>
    right.count === left.count ? left.code.localeCompare(right.code) : right.count - left.count,
  )
})

const visibleWarnings = computed(() => warningGroups.value.slice(0, 3))
const hiddenWarningCount = computed(() =>
  Math.max(0, warningGroups.value.length - visibleWarnings.value.length),
)

const warningSummary = computed<VerificationWarningSummaryItem[]>(() => {
  const grouped = new Map<string, VerificationWarningSummaryItem>()
  for (const warning of warningGroups.value) {
    const current = grouped.get(warning.code)
    if (current) {
      current.count += warning.count
      continue
    }
    grouped.set(warning.code, {
      key: warning.code,
      label: formatWarningCode(warning.code),
      count: warning.count,
    })
  }
  return Array.from(grouped.values()).sort((left, right) =>
    right.count === left.count ? left.label.localeCompare(right.label) : right.count - left.count,
  )
})
</script>

<template>
  <section
    class="rr-assistant-verification"
    :class="[
      `rr-assistant-verification--${tone}`,
      { 'rr-assistant-verification--compact': props.compact },
    ]"
  >
    <div class="rr-assistant-verification__copy">
      <span>{{ t('assistant.verification.eyebrow') }}</span>
      <strong>{{ title }}</strong>
      <p>{{ body }}</p>
    </div>
    <div v-if="warningSummary.length > 0" class="rr-assistant-verification__summary">
      <span
        v-for="item in warningSummary"
        :key="item.key"
        class="rr-assistant-verification__summary-pill"
      >
        <strong>{{ item.label }}</strong>
        <span>{{ item.count }}</span>
      </span>
    </div>
    <details v-if="visibleWarnings.length > 0" class="rr-assistant-verification__details">
      <summary>{{ t('assistant.verification.detailsToggle') }}</summary>
      <ul class="rr-assistant-verification__warnings">
        <li v-for="warning in visibleWarnings" :key="warning.key">
          <div class="rr-assistant-verification__warning-head">
            <strong>{{ formatWarningCode(warning.code) }}</strong>
            <span v-if="warning.count > 1" class="rr-assistant-verification__warning-count">
              {{ warning.count }}×
            </span>
          </div>
          <span>{{ formatWarningMessage(warning) }}</span>
        </li>
      </ul>
      <p v-if="hiddenWarningCount > 0" class="rr-assistant-verification__overflow">
        +{{ hiddenWarningCount }} ещё
      </p>
    </details>
  </section>
</template>
