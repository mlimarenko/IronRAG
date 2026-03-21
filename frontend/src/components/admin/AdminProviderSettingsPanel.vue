<script setup lang="ts">
import { computed, reactive, watch } from 'vue'
import { useDisplayFormatters } from 'src/composables/useDisplayFormatters'
import type {
  AdminAiConsoleState,
  CreateAdminCredentialPayload,
} from 'src/models/ui/admin'

const props = defineProps<{
  settings: AdminAiConsoleState
  credentialSaving: boolean
  validatingBindingId: string | null
}>()
const { enumLabel, formatDateTime } = useDisplayFormatters()

const emit = defineEmits<{
  createCredential: [payload: CreateAdminCredentialPayload]
  validateBinding: [bindingId: string]
}>()

const credentialForm = reactive<CreateAdminCredentialPayload>({
  workspaceId: '',
  providerCatalogId: '',
  label: '',
  secretRef: '',
})

watch(
  () => props.settings,
  (settings) => {
    credentialForm.workspaceId = settings.workspaceId
    credentialForm.providerCatalogId = settings.providers[0]?.id ?? ''
  },
  { immediate: true },
)

const providerMap = computed(
  () => new Map(props.settings.providers.map((provider) => [provider.id, provider])),
)
const presetMap = computed(
  () => new Map(props.settings.modelPresets.map((preset) => [preset.id, preset])),
)

const modelsByProviderId = computed(() => {
  const entries = new Map<string, number>()
  for (const model of props.settings.models) {
    entries.set(model.providerCatalogId, (entries.get(model.providerCatalogId) ?? 0) + 1)
  }
  return entries
})

const credentialRows = computed(() =>
  props.settings.credentials.map((credential) => ({
    ...credential,
    provider: providerMap.value.get(credential.providerCatalogId),
  })),
)

const bindingRows = computed(() =>
  props.settings.bindings.map((binding) => {
    const credential = props.settings.credentials.find(
      (item) => item.id === binding.providerCredentialId,
    )
    const provider = credential ? providerMap.value.get(credential.providerCatalogId) : null
    return {
      ...binding,
      credential,
      provider,
      preset: presetMap.value.get(binding.modelPresetId) ?? null,
    }
  }),
)

const canCreateCredential = computed(
  () =>
    credentialForm.label.trim().length > 0 &&
    credentialForm.secretRef.trim().length > 0 &&
    credentialForm.providerCatalogId.length > 0,
)

function submitCredential(): void {
  if (!canCreateCredential.value) {
    return
  }

  emit('createCredential', {
    workspaceId: credentialForm.workspaceId,
    providerCatalogId: credentialForm.providerCatalogId,
    label: credentialForm.label.trim(),
    secretRef: credentialForm.secretRef.trim(),
  })
  credentialForm.label = ''
  credentialForm.secretRef = ''
}
</script>

<template>
  <section class="rr-page-card rr-admin-settings">
    <header class="rr-admin-settings__header">
      <div>
        <h3>{{ $t('admin.aiCatalog.title') }}</h3>
        <p>
          {{ $t('admin.aiCatalog.subtitle', {
            workspace: settings.workspaceName,
            library: settings.libraryName,
          }) }}
        </p>
      </div>
      <span class="rr-status-pill is-configured">
        {{ $t('admin.aiCatalog.seededCatalog') }}
      </span>
    </header>

    <div class="rr-admin-settings__layout">
      <section class="rr-admin-settings__stack-card">
        <div class="rr-admin-settings__section-head">
          <div>
            <h4>{{ $t('admin.aiCatalog.providersTitle') }}</h4>
            <p>{{ $t('admin.aiCatalog.providersSubtitle') }}</p>
          </div>
        </div>

        <table>
          <thead>
            <tr>
              <th>{{ $t('admin.headers.provider') }}</th>
              <th>{{ $t('admin.headers.apiStyle') }}</th>
              <th>{{ $t('admin.headers.models') }}</th>
              <th>{{ $t('admin.headers.state') }}</th>
            </tr>
          </thead>
          <tbody>
            <tr
              v-for="provider in settings.providers"
              :key="provider.id"
            >
              <td>{{ provider.displayName }}</td>
              <td><code>{{ enumLabel('admin.aiCatalog.apiStyles', provider.apiStyle) }}</code></td>
              <td>{{ modelsByProviderId.get(provider.id) ?? 0 }}</td>
              <td>{{ enumLabel('admin.aiCatalog.providerStates', provider.lifecycleState) }}</td>
            </tr>
          </tbody>
        </table>
      </section>

      <section class="rr-admin-settings__stack-card">
        <div class="rr-admin-settings__section-head">
          <div>
            <h4>{{ $t('admin.aiCatalog.credentialsTitle') }}</h4>
            <p>{{ $t('admin.aiCatalog.credentialsSubtitle', { workspace: settings.workspaceName }) }}</p>
          </div>
        </div>

        <div class="rr-admin-settings__form-grid">
          <label class="rr-field">
            <span>{{ $t('admin.headers.provider') }}</span>
            <select v-model="credentialForm.providerCatalogId">
              <option
                v-for="provider in settings.providers"
                :key="provider.id"
                :value="provider.id"
              >
                {{ provider.displayName }}
              </option>
            </select>
          </label>
          <label class="rr-field">
            <span>{{ $t('admin.headers.label') }}</span>
            <input
              v-model="credentialForm.label"
              type="text"
            >
          </label>
          <label class="rr-field rr-admin-settings__wide-field">
            <span>{{ $t('admin.aiCatalog.secretRef') }}</span>
            <input
              v-model="credentialForm.secretRef"
              type="text"
              placeholder="secret://workspace/provider"
            >
          </label>
        </div>

        <div class="rr-admin-settings__actions">
          <button
            class="rr-button"
            type="button"
            :disabled="!canCreateCredential || credentialSaving"
            @click="submitCredential"
          >
            {{
              credentialSaving
                ? $t('admin.aiCatalog.creatingCredential')
                : $t('admin.aiCatalog.createCredential')
            }}
          </button>
        </div>

        <table v-if="credentialRows.length > 0">
          <thead>
            <tr>
              <th>{{ $t('admin.headers.label') }}</th>
              <th>{{ $t('admin.headers.provider') }}</th>
              <th>{{ $t('admin.headers.secretRef') }}</th>
              <th>{{ $t('admin.headers.state') }}</th>
              <th>{{ $t('admin.headers.updated') }}</th>
            </tr>
          </thead>
          <tbody>
            <tr
              v-for="credential in credentialRows"
              :key="credential.id"
            >
              <td>{{ credential.label }}</td>
              <td>{{ credential.provider?.displayName ?? '—' }}</td>
              <td><code>{{ credential.secretRef }}</code></td>
              <td>{{ enumLabel('admin.aiCatalog.credentialStates', credential.credentialState) }}</td>
              <td>{{ formatDateTime(credential.updatedAt) }}</td>
            </tr>
          </tbody>
        </table>
        <p
          v-else
          class="rr-admin-table__empty"
        >
          {{ $t('admin.aiCatalog.emptyCredentials') }}
        </p>
      </section>

      <section class="rr-admin-settings__stack-card">
        <div class="rr-admin-settings__section-head">
          <div>
            <h4>{{ $t('admin.aiCatalog.bindingsTitle') }}</h4>
            <p>{{ $t('admin.aiCatalog.bindingsSubtitle', { library: settings.libraryName }) }}</p>
          </div>
        </div>

        <div class="rr-admin-settings__callout is-warning">
          <strong>{{ $t('admin.aiCatalog.bindingsNoteTitle') }}</strong>
          <p>{{ $t('admin.aiCatalog.bindingsNoteBody') }}</p>
        </div>

        <table v-if="bindingRows.length > 0">
          <thead>
            <tr>
              <th>{{ $t('admin.headers.purpose') }}</th>
              <th>{{ $t('admin.headers.provider') }}</th>
              <th>{{ $t('admin.headers.credential') }}</th>
              <th>{{ $t('admin.headers.preset') }}</th>
              <th>{{ $t('admin.headers.validation') }}</th>
              <th>{{ $t('admin.headers.actions') }}</th>
            </tr>
          </thead>
          <tbody>
            <tr
              v-for="binding in bindingRows"
              :key="binding.id"
            >
              <td>{{ enumLabel('admin.aiCatalog.bindingPurposes', binding.bindingPurpose) }}</td>
              <td>{{ binding.provider?.displayName ?? '—' }}</td>
              <td>{{ binding.credential?.label ?? '—' }}</td>
              <td>
                <span v-if="binding.preset">{{ binding.preset.presetName }}</span>
                <span v-else>—</span>
              </td>
              <td>
                <div class="rr-admin-token-status">
                  <span>
                    {{
                      binding.latestValidation
                        ? enumLabel(
                            'admin.aiCatalog.validationStates',
                            binding.latestValidation.validationState,
                          )
                        : enumLabel('admin.aiCatalog.bindingStates', binding.bindingState)
                    }}
                  </span>
                  <small v-if="binding.latestValidation">
                    {{ formatDateTime(binding.latestValidation.checkedAt) }}
                  </small>
                </div>
              </td>
              <td>
                <button
                  class="rr-button rr-button--ghost rr-button--tiny"
                  type="button"
                  :disabled="validatingBindingId === binding.id"
                  @click="emit('validateBinding', binding.id)"
                >
                  {{
                    validatingBindingId === binding.id
                      ? $t('admin.aiCatalog.validatingBinding')
                      : $t('admin.aiCatalog.validateBinding')
                  }}
                </button>
              </td>
            </tr>
          </tbody>
        </table>
        <p
          v-else
          class="rr-admin-table__empty"
        >
          {{ $t('admin.aiCatalog.emptyBindings') }}
        </p>
      </section>
    </div>
  </section>
</template>
