<script setup lang="ts">
import type { WebBoundaryPolicy, WebIngestMode } from 'src/models/ui/documents'

const props = withDefaults(
  defineProps<{
    mode: WebIngestMode
    boundaryPolicy: WebBoundaryPolicy
    maxDepth: number
    maxPages: number
    recursiveEnabled?: boolean
  }>(),
  {
    recursiveEnabled: false,
  },
)

const emit = defineEmits<{
  'update:boundaryPolicy': [value: WebBoundaryPolicy]
  'update:maxDepth': [value: number]
  'update:maxPages': [value: number]
}>()
</script>

<template>
  <section class="rr-web-crawl-settings">
    <div
      v-if="props.mode === 'single_page'"
      class="rr-web-crawl-settings__note rr-web-settings-panel"
    >
      <strong>{{ $t('documents.dialogs.addLink.singlePageTitle') }}</strong>
      <p>{{ $t('documents.dialogs.addLink.singlePageDescription') }}</p>
    </div>

    <div
      v-else-if="!props.recursiveEnabled"
      class="rr-web-crawl-settings__note rr-web-settings-panel is-disabled"
    >
      <strong>{{ $t('documents.dialogs.addLink.recursiveDisabledTitle') }}</strong>
      <p>{{ $t('documents.dialogs.addLink.recursiveDisabledDescription') }}</p>
    </div>

    <div v-else class="rr-web-crawl-settings__grid rr-web-settings-panel">
      <div class="rr-field">
        <label for="add-link-boundary-policy">{{
          $t('documents.dialogs.addLink.boundaryPolicyLabel')
        }}</label>
        <select
          id="add-link-boundary-policy"
          :value="props.boundaryPolicy"
          @change="
            emit(
              'update:boundaryPolicy',
              ($event.target as HTMLSelectElement).value as WebBoundaryPolicy,
            )
          "
        >
          <option value="same_host">
            {{ $t('documents.dialogs.addLink.boundaryPolicies.same_host') }}
          </option>
          <option value="allow_external">
            {{ $t('documents.dialogs.addLink.boundaryPolicies.allow_external') }}
          </option>
        </select>
      </div>

      <div class="rr-field">
        <label for="add-link-max-depth">{{ $t('documents.dialogs.addLink.maxDepthLabel') }}</label>
        <input
          id="add-link-max-depth"
          type="number"
          min="0"
          max="8"
          :value="props.maxDepth"
          @input="
            emit(
              'update:maxDepth',
              Math.max(0, Number.parseInt(($event.target as HTMLInputElement).value, 10) || 0),
            )
          "
        />
      </div>

      <div class="rr-field">
        <label for="add-link-max-pages">{{ $t('documents.dialogs.addLink.maxPagesLabel') }}</label>
        <input
          id="add-link-max-pages"
          type="number"
          min="1"
          max="1000"
          :value="props.maxPages"
          @input="
            emit(
              'update:maxPages',
              Math.max(1, Number.parseInt(($event.target as HTMLInputElement).value, 10) || 1),
            )
          "
        />
      </div>
    </div>
  </section>
</template>

<style scoped lang="scss">
.rr-web-crawl-settings {
  display: grid;
  gap: 12px;
}

.rr-web-crawl-settings__grid {
  display: grid;
  gap: 12px;
}

.rr-web-crawl-settings__note {
  display: grid;
  gap: 6px;
  padding: 12px 14px;
  border: 1px solid rgba(191, 219, 254, 0.9);
  border-radius: 14px;
  background:
    linear-gradient(135deg, rgba(239, 246, 255, 0.96), rgba(248, 250, 252, 0.95)),
    rgba(255, 255, 255, 0.92);
}

.rr-web-crawl-settings__note.is-disabled {
  border-color: rgba(226, 232, 240, 0.9);
  background:
    linear-gradient(135deg, rgba(248, 250, 252, 0.98), rgba(241, 245, 249, 0.94)),
    rgba(255, 255, 255, 0.92);
}

.rr-web-crawl-settings__note strong,
.rr-web-crawl-settings__note p {
  margin: 0;
}

.rr-web-crawl-settings__note strong {
  color: rgba(15, 23, 42, 0.88);
  font-size: 0.86rem;
  font-weight: 700;
}

.rr-web-crawl-settings__note p {
  color: rgba(15, 23, 42, 0.64);
  font-size: 0.9rem;
  line-height: 1.5;
}

@media (min-width: 760px) {
  .rr-web-crawl-settings__grid {
    grid-template-columns: repeat(3, minmax(0, 1fr));
  }
}
</style>
