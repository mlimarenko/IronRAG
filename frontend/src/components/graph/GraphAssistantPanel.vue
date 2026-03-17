<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import GraphNodeDetailsCard from 'src/components/graph/GraphNodeDetailsCard.vue'
import type {
  GraphAssistantConfig,
  GraphAssistantMessage,
  GraphAssistantReference,
  GraphAssistantState,
  GraphConvergenceStatus,
  GraphNodeDetail,
  GraphQueryMode,
} from 'src/models/ui/graph'

const props = defineProps<{
  assistant: GraphAssistantState
  assistantConfig: GraphAssistantConfig | null
  draft: string
  mode: GraphQueryMode
  error: string | null
  submitting: boolean
  focusedNodeId: string | null
  focusedNodeLabel: string | null
  focusedDetail: GraphNodeDetail | null
  detailLoading: boolean
  detailError: string | null
  convergenceStatus: GraphConvergenceStatus | null
  activeBlockers: string[]
}>()

const emit = defineEmits<{
  updateDraft: [value: string]
  updateMode: [value: GraphQueryMode]
  submit: [question: string]
  selectNode: [id: string]
  clearFocus: []
}>()

const { t } = useI18n()

const fallbackModes: GraphQueryMode[] = ['document', 'local', 'global', 'hybrid', 'mix']
const fallbackPromptKeys = [
  'graph.defaultPrompts.connectedEntities',
  'graph.defaultPrompts.topEvidence',
  'graph.defaultPrompts.mainThemes',
  'graph.defaultPrompts.isolatedItems',
]

const fallbackModeDescriptors = computed(() =>
  fallbackModes.map((mode) => ({
    mode,
    labelKey: `graph.queryModes.${mode}`,
    shortDescriptionKey: `graph.queryModeHelp.${mode}.description`,
    bestForKey: `graph.queryModeHelp.${mode}.bestFor`,
    cautionKey: `graph.queryModeHelp.${mode}.caution`,
    exampleQuestionKey: `graph.queryModeHelp.${mode}.example`,
  })),
)

const modeDescriptors = computed(() => props.assistantConfig?.modes ?? fallbackModeDescriptors.value)
const availableModes = computed(() => modeDescriptors.value.map((descriptor) => descriptor.mode))
const activeModeDescriptor = computed(
  () => modeDescriptors.value.find((descriptor) => descriptor.mode === props.mode) ?? null,
)

const promptSuggestions = computed(() =>
  (props.assistantConfig?.defaultPromptKeys ?? fallbackPromptKeys).map((key) => t(key)),
)

const scopeHint = computed(() => {
  const key = props.assistantConfig?.scopeHintKey
  return key ? t(key) : t('graph.assistantSubtitle')
})
const convergenceHint = computed(() => {
  if (props.convergenceStatus === 'partial') {
    return props.activeBlockers[0] ?? t('graph.assistantConvergenceHint.partial')
  }
  if (props.convergenceStatus === 'degraded') {
    return t('graph.assistantConvergenceHint.degraded')
  }
  return null
})

const visibleMessages = computed(() => props.assistant.messages.slice(-6))

function submit(): void {
  if (!props.draft.trim()) {
    return
  }
  emit('submit', props.draft.trim())
}

function normalizeReferenceExcerpt(value: string | null): string | null {
  if (!value) {
    return null
  }

  const normalized = value
    .replace(/<[^>]+>/g, ' ')
    .replace(/&[a-z]+;/gi, ' ')
    .replace(/\[[^\]]*]\([^)]+\)/g, ' ')
    .replace(/https?:\/\/\S+/gi, ' ')
    .replace(/[`*_>#-]+/g, ' ')
    .replace(/[│├┤┬┴─\\]+/g, ' ')
    .replace(/\s+/g, ' ')
    .trim()

  if (!normalized) {
    return null
  }

  const compact = normalized.length > 132 ? `${normalized.slice(0, 129).trimEnd()}...` : normalized
  return /^[0-9a-f-]{30,}$/i.test(compact) ? null : compact
}

function fallbackReferenceLabel(referenceId: string, index: number): string {
  if (/^[0-9a-f-]{30,}$/i.test(referenceId)) {
    return `${t('graph.referenceFallback')} #${String(index + 1)}`
  }

  const normalized = referenceId
    .replace(/https?:\/\/\S+/gi, ' ')
    .replace(/[│├┤┬┴─\\]+/g, ' ')
    .replace(/\s+/g, ' ')
    .trim()

  if (normalized && normalized.length <= 84 && !/<[a-z!/]/i.test(normalized)) {
    return normalized
  }

  return `${t('graph.referenceFallback')} #${String(index + 1)}`
}

function referenceLabel(reference: GraphAssistantReference, index: number): string {
  return normalizeReferenceExcerpt(reference.excerpt) ?? fallbackReferenceLabel(reference.referenceId, index)
}

function uniqueReferences(message: GraphAssistantMessage): GraphAssistantReference[] {
  const seen = new Set<string>()
  const unique: GraphAssistantReference[] = []

  for (const reference of message.references) {
    const key = `${reference.kind}:${reference.referenceId}`
    if (seen.has(key)) {
      continue
    }
    seen.add(key)
    unique.push(reference)
  }

  return unique
}

function groupedReferences(message: GraphAssistantMessage): {
  kind: GraphAssistantReference['kind']
  items: GraphAssistantReference[]
}[] {
  const groups = new Map<string, GraphAssistantReference[]>()

  for (const reference of uniqueReferences(message)) {
    const current = groups.get(reference.kind) ?? []
    current.push(reference)
    groups.set(reference.kind, current)
  }

  return ['chunk', 'node', 'edge']
    .filter((kind) => groups.has(kind))
    .map((kind) => ({
      kind,
      items: (groups.get(kind) ?? []).sort((left, right) => left.rank - right.rank),
    }))
}

function referenceCount(message: GraphAssistantMessage): number {
  return uniqueReferences(message).length
}

function canOpenReference(reference: GraphAssistantReference): boolean {
  return reference.kind === 'node'
}

function openReference(reference: GraphAssistantReference): void {
  if (!canOpenReference(reference)) {
    return
  }
  emit('selectNode', reference.referenceId)
}

function showPlannedMode(message: GraphAssistantMessage): boolean {
  return !!message.planning && !!message.mode && message.planning.plannedMode !== message.mode
}

function showIntentReuse(message: GraphAssistantMessage): boolean {
  return !!message.planning && message.planning.intentCacheStatus !== 'miss'
}

function showRerankStatus(message: GraphAssistantMessage): boolean {
  return !!message.rerank && ['applied', 'failed'].includes(message.rerank.status)
}

function showContextStatus(message: GraphAssistantMessage): boolean {
  return !!message.contextAssembly && (message.mode === 'hybrid' || message.mode === 'mix')
}

function showConvergenceWarningPill(message: GraphAssistantMessage): boolean {
  return message.warningKind === 'partial_convergence' && !!message.warning
}

function showInlineWarning(message: GraphAssistantMessage): boolean {
  return !!message.warning && message.warningKind !== 'partial_convergence'
}
</script>

<template>
  <aside class="rr-graph-assistant">
    <header class="rr-graph-assistant__header">
      <div>
        <h3>{{ $t('graph.assistantTitle') }}</h3>
        <p>{{ scopeHint }}</p>
      </div>
    </header>

    <p
      v-if="convergenceHint"
      class="rr-graph-assistant__hint rr-graph-assistant__hint--subtle"
    >
      {{ convergenceHint }}
    </p>

    <div class="rr-graph-assistant__mode-strip">
      <div class="rr-graph-assistant__modes">
        <button
          v-for="entry in availableModes"
          :key="entry"
          type="button"
          class="rr-graph-assistant__mode-pill"
          :class="{ 'is-active': props.mode === entry }"
          :disabled="props.submitting"
          @click="emit('updateMode', entry)"
        >
          {{ $t(`graph.queryModes.${entry}`) }}
        </button>
      </div>
      <button
        v-if="activeModeDescriptor"
        type="button"
        class="rr-graph-assistant__mode-example"
        @click="emit('submit', $t(activeModeDescriptor.exampleQuestionKey))"
      >
        {{ $t(activeModeDescriptor.exampleQuestionKey) }}
      </button>
    </div>

    <section
      v-if="activeModeDescriptor"
      class="rr-graph-assistant__context-card"
    >
      <div>
        <strong>{{ $t(activeModeDescriptor.labelKey) }}</strong>
        <p>{{ $t(activeModeDescriptor.shortDescriptionKey) }}</p>
      </div>
      <div class="rr-graph-assistant__context-meta">
        <span>{{ $t('graph.modeGuide.bestForLabel') }}</span>
        <p>{{ $t(activeModeDescriptor.bestForKey) }}</p>
      </div>
    </section>

    <section
      v-if="props.focusedNodeId || props.focusedDetail || props.detailLoading || props.detailError"
      class="rr-graph-assistant__selected-panel"
    >
      <div class="rr-graph-assistant__selected-header">
        <div>
          <strong>{{ $t('graph.selectedNode') }}</strong>
          <p>{{ props.focusedDetail?.label ?? props.focusedNodeLabel ?? $t('graph.selectedNodePending') }}</p>
        </div>
        <button
          type="button"
          class="rr-button rr-button--ghost rr-button--tiny"
          @click="emit('clearFocus')"
        >
          {{ $t('graph.clearFocus') }}
        </button>
      </div>
      <p
        v-if="props.detailError"
        class="rr-graph-assistant__error rr-graph-assistant__error--inline"
      >
        {{ props.detailError }}
      </p>
      <GraphNodeDetailsCard
        :detail="props.focusedDetail"
        :loading="props.detailLoading"
        @select-node="emit('selectNode', $event)"
      />
    </section>

    <div class="rr-graph-assistant__thread">
      <div
        v-if="visibleMessages.length"
        class="rr-graph-assistant__messages"
      >
        <article
          v-for="message in visibleMessages"
          :key="message.id"
          class="rr-graph-assistant__message"
          :class="`is-${message.role}`"
        >
          <div class="rr-graph-assistant__avatar">
            {{ message.role === 'user' ? $t('graph.youShort') : 'AI' }}
          </div>
          <div class="rr-graph-assistant__bubble-wrap">
            <div class="rr-graph-assistant__bubble">
              <strong>{{ message.role === 'user' ? $t('graph.you') : $t('graph.assistant') }}</strong>
              <p>{{ message.content }}</p>
            </div>

            <div
              v-if="message.role === 'assistant' && (message.mode || message.groundingStatus || message.provider)"
              class="rr-graph-assistant__meta"
            >
              <span
                v-if="message.mode"
                class="rr-graph-assistant__meta-pill"
              >
                {{ $t('graph.queryModes.' + message.mode) }}
              </span>
              <span
                v-if="message.groundingStatus"
                class="rr-graph-assistant__meta-pill"
              >
                {{ $t('graph.grounding.' + message.groundingStatus) }}
              </span>
              <span
                v-if="message.provider"
                class="rr-graph-assistant__meta-pill"
              >
                {{ message.provider.providerKind }} · {{ message.provider.modelName }}
              </span>
              <span
                v-if="showPlannedMode(message)"
                class="rr-graph-assistant__meta-pill"
              >
                {{ $t('graph.assistantMeta.plannedModeLabel') }}:
                {{ $t('graph.queryModes.' + (message.planning?.plannedMode ?? message.mode)) }}
              </span>
              <span
                v-if="showIntentReuse(message)"
                class="rr-graph-assistant__meta-pill"
              >
                {{ $t('graph.assistantMeta.intentCache.' + message.planning?.intentCacheStatus) }}
              </span>
              <span
                v-if="showRerankStatus(message)"
                class="rr-graph-assistant__meta-pill"
              >
                {{
                  message.rerank?.status === 'applied'
                    ? $t('graph.assistantMeta.rerank.applied', { count: message.rerank.reorderedCount ?? 0 })
                    : $t('graph.assistantMeta.rerank.failed')
                }}
              </span>
              <span
                v-if="showContextStatus(message)"
                class="rr-graph-assistant__meta-pill"
              >
                {{ $t('graph.assistantMeta.contextStatus.' + message.contextAssembly?.status) }}
              </span>
              <span
                v-if="showConvergenceWarningPill(message)"
                class="rr-graph-assistant__meta-pill rr-graph-assistant__meta-pill--warning"
                :title="message.warning ?? undefined"
              >
                {{ $t('graph.assistantMeta.warning.partialConvergence') }}
              </span>
            </div>

            <p
              v-if="message.contextAssembly?.status === 'mixed_skewed'"
              class="rr-graph-assistant__hint rr-graph-assistant__hint--compact"
            >
              {{ $t('graph.assistantMeta.contextWarning.mixedSkewed') }}
            </p>

            <details
              v-if="message.role === 'assistant' && referenceCount(message)"
              class="rr-graph-assistant__evidence"
            >
              <summary>
                <strong>{{ $t('graph.groundedSources') }}</strong>
                <span>{{ $t('graph.usedSources', { count: referenceCount(message) }) }}</span>
              </summary>

              <div class="rr-graph-assistant__evidence-groups">
                <section
                  v-for="group in groupedReferences(message)"
                  :key="group.kind"
                  class="rr-graph-assistant__evidence-group"
                >
                  <span class="rr-graph-assistant__evidence-label">
                    {{ $t(`graph.referenceKinds.${group.kind}`) }}
                  </span>
                  <div class="rr-graph-assistant__source-list">
                    <button
                      v-for="(reference, index) in group.items"
                      :key="`${reference.kind}:${reference.referenceId}`"
                      type="button"
                      class="rr-graph-assistant__source-chip"
                      :class="{ 'is-clickable': canOpenReference(reference) }"
                      @click="openReference(reference)"
                    >
                      <span>{{ referenceLabel(reference, index) }}</span>
                    </button>
                  </div>
                </section>
              </div>
            </details>

            <p
              v-if="showInlineWarning(message)"
              class="rr-graph-assistant__warning"
            >
              {{ message.warning }}
            </p>
          </div>
        </article>
      </div>

      <div
        v-else
        class="rr-graph-assistant__empty"
      >
        <p>{{ $t('graph.assistantEmpty') }}</p>
        <button
          v-for="prompt in promptSuggestions.slice(0, 3)"
          :key="prompt"
          class="rr-graph-assistant__prompt"
          type="button"
          @click="emit('submit', prompt)"
        >
          {{ prompt }}
        </button>
      </div>
    </div>

    <p
      v-if="props.error"
      class="rr-graph-assistant__error"
    >
      {{ props.error }}
    </p>

    <footer class="rr-graph-assistant__composer">
      <div
        v-if="!visibleMessages.length"
        class="rr-graph-assistant__composer-prompts"
      >
        <button
          v-for="prompt in promptSuggestions.slice(0, 2)"
          :key="prompt"
          type="button"
          class="rr-graph-assistant__composer-prompt"
          @click="emit('submit', prompt)"
        >
          {{ prompt }}
        </button>
      </div>

      <textarea
        :value="props.draft"
        rows="3"
        :placeholder="$t('graph.askPlaceholder')"
        @input="emit('updateDraft', ($event.target as HTMLTextAreaElement).value)"
        @keydown.enter.exact.prevent="submit"
      />

      <div class="rr-graph-assistant__composer-bar">
        <span>{{ activeModeDescriptor ? $t(activeModeDescriptor.labelKey) : $t('graph.queryModes.hybrid') }}</span>
        <button
          class="rr-button"
          type="button"
          :disabled="props.submitting"
          @click="submit"
        >
          {{ $t('graph.ask') }}
        </button>
      </div>
    </footer>
  </aside>
</template>
