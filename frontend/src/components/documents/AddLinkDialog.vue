<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import type {
  CreateWebIngestRunInput,
  WebBoundaryPolicy,
  WebIngestMode,
} from 'src/models/ui/documents'
import DocumentDialogShell from './DocumentDialogShell.vue'
import WebCrawlSettingsPanel from './WebCrawlSettingsPanel.vue'

const props = withDefaults(
  defineProps<{
    open: boolean
    libraryId: string | null
    loading: boolean
    error?: string | null
    recursiveEnabled?: boolean
  }>(),
  {
    error: null,
    recursiveEnabled: false,
  },
)

const emit = defineEmits<{
  close: []
  submit: [payload: Omit<CreateWebIngestRunInput, 'libraryId'>]
}>()

const seedUrl = ref('')
const mode = ref<WebIngestMode>('single_page')
const boundaryPolicy = ref<WebBoundaryPolicy>('same_host')
const maxDepth = ref(3)
const maxPages = ref(100)
const touched = ref(false)
const isRecursiveMode = computed(() => mode.value === 'recursive_crawl')

const isUrlValid = computed(() => {
  const value = seedUrl.value.trim()
  if (!value) {
    return false
  }
  try {
    const parsed = new URL(value)
    return parsed.protocol === 'http:' || parsed.protocol === 'https:'
  } catch {
    return false
  }
})

const validationError = computed(() => {
  if (!touched.value) {
    return null
  }
  if (!seedUrl.value.trim()) {
    return 'required'
  }
  if (!isUrlValid.value) {
    return 'invalid'
  }
  if (!props.libraryId) {
    return 'library'
  }
  if (mode.value === 'recursive_crawl' && !props.recursiveEnabled) {
    return 'recursive_disabled'
  }
  return null
})

const canSubmit = computed(() => validationError.value === null)

watch(
  () => props.open,
  (open) => {
    if (!open) {
      seedUrl.value = ''
      mode.value = 'single_page'
      boundaryPolicy.value = 'same_host'
      maxDepth.value = 3
      maxPages.value = 100
      touched.value = false
    }
  },
)

function submit(): void {
  touched.value = true
  if (!canSubmit.value) {
    return
  }
  emit('submit', {
    seedUrl: seedUrl.value.trim(),
    mode: mode.value,
    boundaryPolicy: isRecursiveMode.value ? boundaryPolicy.value : null,
    maxDepth: isRecursiveMode.value ? maxDepth.value : null,
    maxPages: isRecursiveMode.value ? maxPages.value : null,
    idempotencyKey: null,
  })
}
</script>

<template>
  <DocumentDialogShell
    :open="props.open"
    :title="$t('documents.dialogs.addLink.title')"
    :description="$t('documents.dialogs.addLink.description')"
    :submit-label="$t('documents.actions.addLink')"
    :submit-disabled="!canSubmit"
    :loading="props.loading"
    @close="emit('close')"
    @submit="submit"
  >
    <div class="rr-add-link-dialog">
      <div class="rr-field">
        <label for="add-link-seed-url">{{ $t('documents.dialogs.addLink.urlLabel') }}</label>
        <input
          id="add-link-seed-url"
          v-model="seedUrl"
          type="url"
          inputmode="url"
          :placeholder="$t('documents.dialogs.addLink.urlPlaceholder')"
        />
      </div>

      <div class="rr-field">
        <span class="rr-field__label">{{ $t('documents.dialogs.addLink.modeLabel') }}</span>
        <div class="rr-add-link-dialog__mode-grid">
          <label
            class="rr-add-link-dialog__mode-card"
            :class="{ 'is-selected': mode === 'single_page' }"
          >
            <input v-model="mode" type="radio" name="add-link-mode" value="single_page" />
            <span class="rr-add-link-dialog__mode-copy">
              <strong>{{ $t('documents.dialogs.addLink.modes.single_page') }}</strong>
              <small>{{ $t('documents.dialogs.addLink.modeDescriptions.single_page') }}</small>
            </span>
          </label>

          <label
            class="rr-add-link-dialog__mode-card"
            :class="{
              'is-selected': mode === 'recursive_crawl',
              'is-disabled': !props.recursiveEnabled,
            }"
          >
            <input
              v-model="mode"
              type="radio"
              name="add-link-mode"
              value="recursive_crawl"
              :disabled="!props.recursiveEnabled"
            />
            <span class="rr-add-link-dialog__mode-copy">
              <strong>{{ $t('documents.dialogs.addLink.modes.recursive_crawl') }}</strong>
              <small>{{ $t('documents.dialogs.addLink.modeDescriptions.recursive_crawl') }}</small>
            </span>
          </label>
        </div>
      </div>

      <WebCrawlSettingsPanel
        :mode="mode"
        :recursive-enabled="props.recursiveEnabled"
        :boundary-policy="boundaryPolicy"
        :max-depth="maxDepth"
        :max-pages="maxPages"
        @update:boundary-policy="boundaryPolicy = $event"
        @update:max-depth="maxDepth = $event"
        @update:max-pages="maxPages = $event"
      />

      <section class="rr-add-link-dialog__settings-preview rr-web-settings-panel">
        <div class="rr-add-link-dialog__settings-copy">
          <strong>{{ $t('documents.dialogs.addLink.immutableSettingsTitle') }}</strong>
          <p>{{ $t('documents.dialogs.addLink.immutableSettingsDescription') }}</p>
        </div>

        <dl class="rr-add-link-dialog__settings-grid">
          <div class="rr-add-link-dialog__settings-item">
            <dt>{{ $t('documents.webRuns.fields.mode') }}</dt>
            <dd>{{ $t(`documents.dialogs.addLink.modes.${mode}`) }}</dd>
          </div>
          <div class="rr-add-link-dialog__settings-item">
            <dt>{{ $t('documents.webRuns.fields.boundary') }}</dt>
            <dd>
              {{
                isRecursiveMode
                  ? $t(`documents.dialogs.addLink.boundaryPolicies.${boundaryPolicy}`)
                  : $t('documents.dialogs.addLink.notUsed')
              }}
            </dd>
          </div>
          <div class="rr-add-link-dialog__settings-item">
            <dt>{{ $t('documents.webRuns.fields.maxDepth') }}</dt>
            <dd>{{ isRecursiveMode ? maxDepth : $t('documents.dialogs.addLink.notUsed') }}</dd>
          </div>
          <div class="rr-add-link-dialog__settings-item">
            <dt>{{ $t('documents.webRuns.fields.maxPages') }}</dt>
            <dd>{{ isRecursiveMode ? maxPages : $t('documents.dialogs.addLink.notUsed') }}</dd>
          </div>
        </dl>
      </section>

      <p v-if="validationError === 'required'" class="rr-document-dialog__error">
        {{ $t('documents.dialogs.addLink.validationRequired') }}
      </p>
      <p v-else-if="validationError === 'invalid'" class="rr-document-dialog__error">
        {{ $t('documents.dialogs.addLink.validationInvalid') }}
      </p>
      <p v-else-if="validationError === 'library'" class="rr-document-dialog__error">
        {{ $t('documents.dialogs.addLink.validationLibrary') }}
      </p>
      <p v-else-if="validationError === 'recursive_disabled'" class="rr-document-dialog__error">
        {{ $t('documents.dialogs.addLink.recursiveDisabledDescription') }}
      </p>
      <p v-else-if="props.error" class="rr-document-dialog__error">
        {{ props.error }}
      </p>
    </div>
  </DocumentDialogShell>
</template>

<style scoped lang="scss">
.rr-add-link-dialog {
  display: grid;
  gap: 14px;
}

.rr-add-link-dialog__mode-grid {
  display: grid;
  gap: 12px;
}

.rr-add-link-dialog__mode-card {
  display: grid;
  grid-template-columns: auto minmax(0, 1fr);
  gap: 12px;
  align-items: start;
  padding: 14px 16px;
  border: 1px solid rgba(203, 213, 225, 0.9);
  border-radius: 16px;
  background:
    linear-gradient(135deg, rgba(255, 255, 255, 0.98), rgba(248, 250, 252, 0.94)),
    rgba(255, 255, 255, 0.96);
  cursor: pointer;
  transition:
    border-color 160ms ease,
    box-shadow 160ms ease,
    transform 160ms ease;
}

.rr-add-link-dialog__mode-card input {
  margin-top: 2px;
}

.rr-add-link-dialog__mode-card.is-selected {
  border-color: rgba(14, 116, 144, 0.78);
  box-shadow: 0 14px 32px rgba(8, 47, 73, 0.12);
  transform: translateY(-1px);
}

.rr-add-link-dialog__mode-card.is-disabled {
  cursor: not-allowed;
  opacity: 0.68;
}

.rr-add-link-dialog__mode-copy {
  display: grid;
  gap: 4px;
}

.rr-add-link-dialog__mode-copy strong,
.rr-add-link-dialog__mode-copy small {
  display: block;
}

.rr-add-link-dialog__mode-copy strong {
  color: rgba(15, 23, 42, 0.9);
  font-size: 0.92rem;
  font-weight: 700;
}

.rr-add-link-dialog__mode-copy small {
  color: rgba(15, 23, 42, 0.66);
  font-size: 0.86rem;
  line-height: 1.45;
}

.rr-add-link-dialog__settings-preview {
  display: grid;
  gap: 12px;
  padding: 14px 16px;
  border: 1px solid rgba(191, 219, 254, 0.88);
  border-radius: 16px;
  background:
    linear-gradient(135deg, rgba(239, 246, 255, 0.96), rgba(248, 250, 252, 0.95)),
    rgba(255, 255, 255, 0.94);
}

.rr-add-link-dialog__settings-copy {
  display: grid;
  gap: 4px;
}

.rr-add-link-dialog__settings-copy strong,
.rr-add-link-dialog__settings-copy p {
  margin: 0;
}

.rr-add-link-dialog__settings-copy strong {
  color: rgba(15, 23, 42, 0.9);
  font-size: 0.9rem;
  font-weight: 700;
}

.rr-add-link-dialog__settings-copy p {
  color: rgba(15, 23, 42, 0.64);
  font-size: 0.86rem;
  line-height: 1.45;
}

.rr-add-link-dialog__settings-grid {
  display: grid;
  gap: 10px;
  margin: 0;
}

.rr-add-link-dialog__settings-item {
  display: grid;
  gap: 4px;
  padding: 10px 12px;
  border-radius: 12px;
  background: rgba(255, 255, 255, 0.72);
}

.rr-add-link-dialog__settings-item dt,
.rr-add-link-dialog__settings-item dd {
  margin: 0;
}

.rr-add-link-dialog__settings-item dt {
  color: rgba(15, 23, 42, 0.58);
  font-size: 0.77rem;
  font-weight: 600;
  letter-spacing: 0.04em;
  text-transform: uppercase;
}

.rr-add-link-dialog__settings-item dd {
  color: rgba(15, 23, 42, 0.9);
  font-size: 0.92rem;
  font-weight: 600;
}

@media (min-width: 760px) {
  .rr-add-link-dialog__mode-grid,
  .rr-add-link-dialog__settings-grid {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }
}
</style>
