<script setup lang="ts">
import { computed, reactive, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { useDisplayFormatters } from 'src/composables/useDisplayFormatters'
import type {
  AdminAiConsoleState,
  AdminLibraryBinding,
  AdminModelPreset,
  AdminProviderCredential,
  CreateAdminCredentialPayload,
  CreateAdminModelPresetPayload,
  SaveAdminLibraryBindingPayload,
  UpdateAdminCredentialPayload,
  UpdateAdminModelPresetPayload,
} from 'src/models/ui/admin'

type EditableCredentialForm = {
  credentialId: string | null
  workspaceId: string
  providerCatalogId: string
  label: string
  apiKey: string
  credentialState: string
}

type EditablePresetForm = {
  presetId: string | null
  workspaceId: string
  modelCatalogId: string
  presetName: string
  systemPrompt: string
  temperature: string
  topP: string
  maxOutputTokensOverride: string
}

type EditableAssignmentForm = {
  bindingId: string | null
  workspaceId: string
  libraryId: string
  bindingPurpose: string
  providerCredentialId: string
  modelPresetId: string
  bindingState: string
}

type SettingsSection = 'credentials' | 'modelPresets' | 'assignments'
type LibraryTaskPurpose = 'extract_graph' | 'embed_chunk' | 'query_answer' | 'vision'

const LIBRARY_TASKS: LibraryTaskPurpose[] = ['extract_graph', 'embed_chunk', 'query_answer', 'vision']

const props = defineProps<{
  settings: AdminAiConsoleState
  saving: boolean
  validatingBindingId: string | null
  commitVersion: number
  errorMessage?: string | null
  embedded?: boolean
}>()

const emit = defineEmits<{
  createCredential: [payload: CreateAdminCredentialPayload]
  updateCredential: [payload: UpdateAdminCredentialPayload]
  createModelPreset: [payload: CreateAdminModelPresetPayload]
  updateModelPreset: [payload: UpdateAdminModelPresetPayload]
  saveBinding: [payload: SaveAdminLibraryBindingPayload]
  validateBinding: [bindingId: string]
}>()

const { t } = useI18n()
const { bindingPurposeLabel, enumLabel, formatDateTime, providerStateLabel } = useDisplayFormatters()

const credentialForm = reactive<EditableCredentialForm>({
  credentialId: null,
  workspaceId: '',
  providerCatalogId: '',
  label: '',
  apiKey: '',
  credentialState: 'active',
})

const presetForm = reactive<EditablePresetForm>({
  presetId: null,
  workspaceId: '',
  modelCatalogId: '',
  presetName: '',
  systemPrompt: '',
  temperature: '',
  topP: '',
  maxOutputTokensOverride: '',
})

const assignmentForm = reactive<EditableAssignmentForm>({
  bindingId: null,
  workspaceId: '',
  libraryId: '',
  bindingPurpose: 'embed_chunk',
  providerCredentialId: '',
  modelPresetId: '',
  bindingState: 'active',
})

const editingSection = ref<SettingsSection | null>(null)
const selectionKey = ref<string | null>(null)
const pendingSubmitSection = ref<SettingsSection | null>(null)

watch(
  () => props.settings,
  (settings) => {
    credentialForm.workspaceId = settings.workspaceId
    presetForm.workspaceId = settings.workspaceId
    assignmentForm.workspaceId = settings.workspaceId
    assignmentForm.libraryId = settings.libraryId

    if (!settings.providers.some((provider) => provider.id === credentialForm.providerCatalogId)) {
      credentialForm.providerCatalogId = settings.providers[0]?.id ?? ''
    }
    if (!settings.models.some((model) => model.id === presetForm.modelCatalogId)) {
      presetForm.modelCatalogId = settings.models[0]?.id ?? ''
    }
    if (!settings.credentials.some((credential) => credential.id === assignmentForm.providerCredentialId)) {
      assignmentForm.providerCredentialId = settings.credentials[0]?.id ?? ''
    }
    if (!settings.modelPresets.some((preset) => preset.id === assignmentForm.modelPresetId)) {
      assignmentForm.modelPresetId = settings.modelPresets[0]?.id ?? ''
    }
  },
  { immediate: true },
)

watch(
  () => `${props.settings.workspaceId}:${props.settings.libraryId}`,
  (nextKey, previousKey) => {
    if (!previousKey || nextKey === previousKey) {
      return
    }
    selectCredential()
  },
)

const providerMap = computed(
  () => new Map(props.settings.providers.map((provider) => [provider.id, provider])),
)

const modelMap = computed(
  () => new Map(props.settings.models.map((model) => [model.id, model])),
)

const presetMap = computed(
  () => new Map(props.settings.modelPresets.map((preset) => [preset.id, preset])),
)

const providerModelCounts = computed(() => {
  const counts = new Map<string, number>()
  for (const model of props.settings.models) {
    counts.set(model.providerCatalogId, (counts.get(model.providerCatalogId) ?? 0) + 1)
  }
  return counts
})

const credentialRows = computed(() =>
  props.settings.credentials.map((credential) => ({
    ...credential,
    provider: providerMap.value.get(credential.providerCatalogId) ?? null,
    providerModelCount: providerModelCounts.value.get(credential.providerCatalogId) ?? 0,
  })),
)

const presetRows = computed(() =>
  props.settings.modelPresets.map((preset) => {
    const model = modelMap.value.get(preset.modelCatalogId) ?? null
    const provider = model ? providerMap.value.get(model.providerCatalogId) ?? null : null
    return { ...preset, model, provider }
  }),
)

const assignmentRows = computed(() =>
  props.settings.bindings.map((binding) => {
    const credential =
      props.settings.credentials.find((item) => item.id === binding.providerCredentialId) ?? null
    const preset = presetMap.value.get(binding.modelPresetId) ?? null
    const model = preset ? modelMap.value.get(preset.modelCatalogId) ?? null : null
    const provider = credential
      ? providerMap.value.get(credential.providerCatalogId) ?? null
      : model
        ? providerMap.value.get(model.providerCatalogId) ?? null
        : null
    return {
      ...binding,
      credential,
      preset,
      model,
      provider,
    }
  }),
)

const libraryTaskRows = computed(() =>
  LIBRARY_TASKS.map((purpose) => ({
    purpose,
    binding: assignmentRows.value.find((item) => item.bindingPurpose === purpose) ?? null,
  })),
)

const assignmentPresetOptions = computed(() => {
  const selectedCredential = props.settings.credentials.find(
    (credential) => credential.id === assignmentForm.providerCredentialId,
  )
  if (!selectedCredential) {
    return props.settings.modelPresets
  }
  return props.settings.modelPresets.filter((preset) => {
    const model = modelMap.value.get(preset.modelCatalogId)
    return model?.providerCatalogId === selectedCredential.providerCatalogId
  })
})

const activeValidation = computed(() => {
  if (!assignmentForm.bindingId) {
    return null
  }
  return (
    assignmentRows.value.find((binding) => binding.id === assignmentForm.bindingId)?.latestValidation ??
    null
  )
})

const detailErrorMessage = computed(() => {
  if (!editingSection.value) {
    return null
  }
  return props.errorMessage ?? null
})

watch(
  assignmentPresetOptions,
  (options) => {
    if (options.some((preset) => preset.id === assignmentForm.modelPresetId)) {
      return
    }
    assignmentForm.modelPresetId = options[0]?.id ?? ''
  },
  { immediate: true },
)

const canSaveCredential = computed(
  () =>
    credentialForm.providerCatalogId.trim().length > 0 &&
    props.settings.providers.some((provider) => provider.id === credentialForm.providerCatalogId) &&
    credentialForm.label.trim().length > 0 &&
    (credentialForm.credentialId !== null || credentialForm.apiKey.trim().length > 0),
)

const canSavePreset = computed(
  () =>
    presetForm.modelCatalogId.trim().length > 0 &&
    props.settings.models.some((model) => model.id === presetForm.modelCatalogId) &&
    presetForm.presetName.trim().length > 0,
)

const canSaveAssignment = computed(
  () =>
    assignmentForm.bindingPurpose.trim().length > 0 &&
    assignmentForm.providerCredentialId.trim().length > 0 &&
    props.settings.credentials.some(
      (credential) => credential.id === assignmentForm.providerCredentialId,
    ) &&
    assignmentForm.modelPresetId.trim().length > 0 &&
    assignmentPresetOptions.value.some((preset) => preset.id === assignmentForm.modelPresetId),
)

function parseOptionalNumber(value: string): number | null {
  const normalized = value.trim()
  if (!normalized) {
    return null
  }
  const parsed = Number(normalized)
  return Number.isFinite(parsed) ? parsed : null
}

function parseOptionalInteger(value: string): number | null {
  const parsed = parseOptionalNumber(value)
  return parsed === null ? null : Math.trunc(parsed)
}

function resetCredentialForm(): void {
  credentialForm.credentialId = null
  credentialForm.providerCatalogId = props.settings.providers[0]?.id ?? ''
  credentialForm.label = ''
  credentialForm.apiKey = ''
  credentialForm.credentialState = 'active'
}

function resetPresetForm(): void {
  presetForm.presetId = null
  presetForm.modelCatalogId = props.settings.models[0]?.id ?? ''
  presetForm.presetName = ''
  presetForm.systemPrompt = ''
  presetForm.temperature = ''
  presetForm.topP = ''
  presetForm.maxOutputTokensOverride = ''
}

function resetAssignmentForm(): void {
  assignmentForm.bindingId = null
  assignmentForm.bindingPurpose = 'embed_chunk'
  assignmentForm.providerCredentialId = props.settings.credentials[0]?.id ?? ''
  assignmentForm.modelPresetId =
    assignmentPresetOptions.value[0]?.id ?? props.settings.modelPresets[0]?.id ?? ''
  assignmentForm.bindingState = 'active'
}

function selectCredential(credential?: AdminProviderCredential): void {
  editingSection.value = 'credentials'
  selectionKey.value = credential ? `credential:${credential.id}` : 'credential:new'

  if (!credential) {
    resetCredentialForm()
    return
  }

  credentialForm.credentialId = credential.id
  credentialForm.workspaceId = credential.workspaceId
  credentialForm.providerCatalogId = credential.providerCatalogId
  credentialForm.label = credential.label
  credentialForm.apiKey = ''
  credentialForm.credentialState = credential.credentialState
}

function selectPreset(preset?: AdminModelPreset): void {
  editingSection.value = 'modelPresets'
  selectionKey.value = preset ? `preset:${preset.id}` : 'preset:new'

  if (!preset) {
    resetPresetForm()
    return
  }

  presetForm.presetId = preset.id
  presetForm.workspaceId = preset.workspaceId
  presetForm.modelCatalogId = preset.modelCatalogId
  presetForm.presetName = preset.presetName
  presetForm.systemPrompt = preset.systemPrompt ?? ''
  presetForm.temperature = preset.temperature === null ? '' : String(preset.temperature)
  presetForm.topP = preset.topP === null ? '' : String(preset.topP)
  presetForm.maxOutputTokensOverride =
    preset.maxOutputTokensOverride === null ? '' : String(preset.maxOutputTokensOverride)
}

function selectAssignment(taskPurpose: LibraryTaskPurpose): void {
  const binding = assignmentRows.value.find((item) => item.bindingPurpose === taskPurpose) ?? null
  editingSection.value = 'assignments'
  selectionKey.value = binding ? `binding:${binding.id}` : `binding:${taskPurpose}`

  if (!binding) {
    resetAssignmentForm()
    assignmentForm.bindingPurpose = taskPurpose
    return
  }

  assignmentForm.bindingId = binding.id
  assignmentForm.workspaceId = binding.workspaceId
  assignmentForm.libraryId = binding.libraryId
  assignmentForm.bindingPurpose = binding.bindingPurpose
  assignmentForm.providerCredentialId = binding.providerCredentialId
  assignmentForm.modelPresetId = binding.modelPresetId
  assignmentForm.bindingState = binding.bindingState
}

function closeDetail(): void {
  editingSection.value = null
  selectionKey.value = null
  pendingSubmitSection.value = null
  resetCredentialForm()
  resetPresetForm()
  resetAssignmentForm()
  selectCredential()
}

function submitCredential(): void {
  if (!canSaveCredential.value) {
    return
  }

  pendingSubmitSection.value = 'credentials'

  if (credentialForm.credentialId) {
    emit('updateCredential', {
      credentialId: credentialForm.credentialId,
      label: credentialForm.label.trim(),
      apiKey: credentialForm.apiKey.trim() || null,
      credentialState: credentialForm.credentialState,
    })
  } else {
    emit('createCredential', {
      workspaceId: credentialForm.workspaceId,
      providerCatalogId: credentialForm.providerCatalogId,
      label: credentialForm.label.trim(),
      apiKey: credentialForm.apiKey.trim(),
    })
  }
}

function submitPreset(): void {
  if (!canSavePreset.value) {
    return
  }

  pendingSubmitSection.value = 'modelPresets'

  const payload = {
    presetName: presetForm.presetName.trim(),
    systemPrompt: presetForm.systemPrompt.trim() || null,
    temperature: parseOptionalNumber(presetForm.temperature),
    topP: parseOptionalNumber(presetForm.topP),
    maxOutputTokensOverride: parseOptionalInteger(presetForm.maxOutputTokensOverride),
    extraParametersJson: {},
  }

  if (presetForm.presetId) {
    emit('updateModelPreset', {
      presetId: presetForm.presetId,
      ...payload,
    })
  } else {
    emit('createModelPreset', {
      workspaceId: presetForm.workspaceId,
      modelCatalogId: presetForm.modelCatalogId,
      ...payload,
    })
  }
}

function submitAssignment(): void {
  if (!canSaveAssignment.value) {
    return
  }

  pendingSubmitSection.value = 'assignments'

  emit('saveBinding', {
    bindingId: assignmentForm.bindingId ?? undefined,
    workspaceId: assignmentForm.workspaceId,
    libraryId: assignmentForm.libraryId,
    bindingPurpose: assignmentForm.bindingPurpose,
    providerCredentialId: assignmentForm.providerCredentialId,
    modelPresetId: assignmentForm.modelPresetId,
    bindingState: assignmentForm.bindingState,
  })
}

function modelDescriptor(modelCatalogId: string): string {
  const model = modelMap.value.get(modelCatalogId)
  const provider = model ? providerMap.value.get(model.providerCatalogId) : null
  if (!model) {
    return modelCatalogId
  }
  return provider ? `${provider.displayName} · ${model.modelName}` : model.modelName
}

function assignmentValidationLabel(binding: (typeof assignmentRows.value)[number]): string {
  if (binding.latestValidation) {
    return enumLabel('admin.aiCatalog.validationStates', binding.latestValidation.validationState)
  }
  return enumLabel('admin.aiCatalog.bindingStates', binding.bindingState)
}

watch(
  () => props.commitVersion,
  (next, previous) => {
    if (next <= previous || !pendingSubmitSection.value) {
      return
    }
    closeDetail()
  },
)
</script>

<template>
  <section class="rr-admin-ai">
    <div
      class="rr-admin-ai__layout"
      :class="{ 'rr-admin-ai__layout--editing': Boolean(editingSection) }"
    >
      <aside class="rr-admin-ai__rail">
        <div
          v-if="!editingSection"
          class="rr-admin-ai__intro"
        >
          <strong>{{ $t('admin.aiCatalog.editorPromptTitle') }}</strong>
          <p>{{ $t('admin.aiCatalog.editorPromptDescription') }}</p>
        </div>

        <section class="rr-admin-ai__rail-group">
          <header class="rr-admin-ai__rail-head">
            <div>
              <h3>{{ $t('admin.aiCatalog.credentialsTitle') }}</h3>
              <p>{{ $t('admin.aiCatalog.credentialsSubtitle', { workspace: settings.workspaceName }) }}</p>
            </div>
            <button
              class="rr-button rr-button--ghost rr-button--tiny"
              type="button"
              @click="selectCredential()"
            >
              {{ $t('admin.aiCatalog.createCredential') }}
            </button>
          </header>

          <div
            v-if="credentialRows.length"
            class="rr-admin-ai__rail-list"
          >
            <button
              v-for="row in credentialRows"
              :key="row.id"
              class="rr-admin-ai__rail-row"
              :class="{ 'rr-admin-ai__rail-row--active': selectionKey === `credential:${row.id}` }"
              type="button"
              @click="selectCredential(row)"
            >
              <span class="rr-admin-ai__rail-row-title">{{ row.label }}</span>
              <span class="rr-admin-ai__rail-row-meta">
                {{ row.provider?.displayName ?? '—' }} ·
                {{ providerStateLabel(row.credentialState) }} ·
                {{ t('admin.aiCatalog.providerSummary', { models: row.providerModelCount, credentials: 1 }) }}
              </span>
            </button>
          </div>
          <p
            v-else
            class="rr-admin-ai__empty-copy"
          >
            {{ $t('admin.aiCatalog.emptyCredentials') }}
          </p>
        </section>

        <section class="rr-admin-ai__rail-group">
          <header class="rr-admin-ai__rail-head">
            <div>
              <h3>{{ $t('admin.aiCatalog.modelPresetsTitle') }}</h3>
              <p>{{ $t('admin.aiCatalog.modelPresetsSubtitle') }}</p>
            </div>
            <button
              class="rr-button rr-button--ghost rr-button--tiny"
              type="button"
              @click="selectPreset()"
            >
              {{ $t('admin.aiCatalog.createPreset') }}
            </button>
          </header>

          <div
            v-if="presetRows.length"
            class="rr-admin-ai__rail-list"
          >
            <button
              v-for="row in presetRows"
              :key="row.id"
              class="rr-admin-ai__rail-row"
              :class="{ 'rr-admin-ai__rail-row--active': selectionKey === `preset:${row.id}` }"
              type="button"
              @click="selectPreset(row)"
            >
              <span class="rr-admin-ai__rail-row-title">{{ row.presetName }}</span>
              <span class="rr-admin-ai__rail-row-meta">{{ modelDescriptor(row.modelCatalogId) }}</span>
            </button>
          </div>
          <p
            v-else
            class="rr-admin-ai__empty-copy"
          >
            {{ $t('admin.aiCatalog.emptyPresets') }}
          </p>
        </section>

        <section class="rr-admin-ai__rail-group">
          <header class="rr-admin-ai__rail-head">
            <div>
              <h3>{{ $t('admin.aiCatalog.assignmentsTitle') }}</h3>
              <p>{{ $t('admin.aiCatalog.assignmentsSubtitle', { library: settings.libraryName }) }}</p>
            </div>
          </header>

          <div class="rr-admin-ai__rail-list">
            <button
              v-for="task in libraryTaskRows"
              :key="task.purpose"
              class="rr-admin-ai__rail-row"
              :class="{
                'rr-admin-ai__rail-row--active':
                  selectionKey === (task.binding ? `binding:${task.binding.id}` : `binding:${task.purpose}`),
              }"
              type="button"
              @click="selectAssignment(task.purpose)"
            >
              <span class="rr-admin-ai__rail-row-title">
                {{ bindingPurposeLabel(task.purpose) }}
              </span>
              <span
                v-if="task.binding"
                class="rr-admin-ai__rail-row-meta"
              >
                {{ task.binding.credential?.label ?? '—' }} ·
                {{ task.binding.preset?.presetName ?? '—' }}
              </span>
              <span
                v-else
                class="rr-admin-ai__rail-row-meta"
              >
                {{ $t('admin.aiCatalog.unconfiguredTask') }}
              </span>
            </button>
          </div>
        </section>
      </aside>

      <section
        v-if="editingSection"
        class="rr-admin-ai__detail"
      >
        <div
          v-if="editingSection === 'credentials'"
          class="rr-admin-ai__detail-card"
        >
          <header class="rr-admin-ai__detail-head">
            <div>
              <h3>
                {{
                  credentialForm.credentialId
                    ? $t('admin.aiCatalog.updateCredential')
                    : $t('admin.aiCatalog.createCredential')
                }}
              </h3>
              <p>{{ $t('admin.aiCatalog.credentialsSubtitle', { workspace: settings.workspaceName }) }}</p>
            </div>
          </header>

          <p
            v-if="detailErrorMessage"
            class="rr-admin-ai__detail-error"
          >
            {{ detailErrorMessage }}
          </p>

          <div class="rr-admin-ai__form-grid">
            <label class="rr-admin-ai__field rr-admin-ai__field--wide">
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

            <label class="rr-admin-ai__field rr-admin-ai__field--wide">
              <span>{{ $t('admin.headers.label') }}</span>
              <input
                v-model="credentialForm.label"
                type="text"
              >
            </label>

            <label class="rr-admin-ai__field rr-admin-ai__field--wide">
              <span>{{ $t('admin.aiCatalog.apiKeyLabel') }}</span>
              <input
                v-model="credentialForm.apiKey"
                type="password"
                :placeholder="
                  credentialForm.credentialId
                    ? $t('admin.aiCatalog.apiKeyKeepExistingPlaceholder')
                    : $t('admin.aiCatalog.apiKeyPlaceholder')
                "
              >
            </label>

            <label
              v-if="credentialForm.credentialId"
              class="rr-admin-ai__field"
            >
              <span>{{ $t('admin.headers.state') }}</span>
              <select v-model="credentialForm.credentialState">
                <option value="active">{{ enumLabel('admin.aiCatalog.credentialStates', 'active') }}</option>
                <option value="invalid">{{ enumLabel('admin.aiCatalog.credentialStates', 'invalid') }}</option>
                <option value="archived">{{ enumLabel('admin.aiCatalog.credentialStates', 'archived') }}</option>
              </select>
            </label>
          </div>

          <p
            v-if="credentialForm.credentialId"
            class="rr-admin-ai__detail-note"
          >
            {{ $t('admin.aiCatalog.apiKeyKeepExistingHint') }}
          </p>

          <div class="rr-admin-ai__actions">
            <button
              class="rr-button rr-button--ghost"
              type="button"
              @click="closeDetail"
            >
              {{ $t('dialogs.close') }}
            </button>
            <button
              class="rr-button"
              type="button"
              :disabled="!canSaveCredential || saving"
              @click="submitCredential"
            >
              {{
                credentialForm.credentialId
                  ? $t('admin.aiCatalog.updateCredential')
                  : $t('admin.aiCatalog.createCredential')
              }}
            </button>
          </div>
        </div>

        <div
          v-else-if="editingSection === 'modelPresets'"
          class="rr-admin-ai__detail-card"
        >
          <header class="rr-admin-ai__detail-head">
            <div>
              <h3>
                {{
                  presetForm.presetId
                    ? $t('admin.aiCatalog.updatePreset')
                    : $t('admin.aiCatalog.createPreset')
                }}
              </h3>
              <p>{{ $t('admin.aiCatalog.modelPresetsSubtitle') }}</p>
            </div>
          </header>

          <p
            v-if="detailErrorMessage"
            class="rr-admin-ai__detail-error"
          >
            {{ detailErrorMessage }}
          </p>

          <div class="rr-admin-ai__form-grid">
            <label class="rr-admin-ai__field rr-admin-ai__field--wide">
              <span>{{ $t('admin.headers.model') }}</span>
              <select v-model="presetForm.modelCatalogId">
                <option
                  v-for="model in settings.models"
                  :key="model.id"
                  :value="model.id"
                >
                  {{ modelDescriptor(model.id) }}
                </option>
              </select>
            </label>

            <label class="rr-admin-ai__field rr-admin-ai__field--wide">
              <span>{{ $t('admin.headers.preset') }}</span>
              <input
                v-model="presetForm.presetName"
                type="text"
              >
            </label>

            <label class="rr-admin-ai__field rr-admin-ai__field--wide">
              <span>{{ $t('admin.aiCatalog.systemPrompt') }}</span>
              <textarea v-model="presetForm.systemPrompt" />
            </label>

            <label class="rr-admin-ai__field">
              <span>{{ $t('admin.aiCatalog.temperature') }}</span>
              <input
                v-model="presetForm.temperature"
                type="number"
                step="0.1"
              >
            </label>

            <label class="rr-admin-ai__field">
              <span>{{ $t('admin.aiCatalog.topP') }}</span>
              <input
                v-model="presetForm.topP"
                type="number"
                step="0.1"
              >
            </label>

            <label class="rr-admin-ai__field">
              <span>{{ $t('admin.aiCatalog.maxOutputTokens') }}</span>
              <input
                v-model="presetForm.maxOutputTokensOverride"
                type="number"
                min="1"
              >
            </label>
          </div>

          <div class="rr-admin-ai__actions">
            <button
              class="rr-button rr-button--ghost"
              type="button"
              @click="closeDetail"
            >
              {{ $t('dialogs.close') }}
            </button>
            <button
              class="rr-button"
              type="button"
              :disabled="!canSavePreset || saving"
              @click="submitPreset"
            >
              {{
                presetForm.presetId
                  ? $t('admin.aiCatalog.updatePreset')
                  : $t('admin.aiCatalog.createPreset')
              }}
            </button>
          </div>
        </div>

        <div
          v-else
          class="rr-admin-ai__detail-card"
        >
          <header class="rr-admin-ai__detail-head">
            <div>
              <h3>{{ bindingPurposeLabel(assignmentForm.bindingPurpose) }}</h3>
              <p>{{ $t('admin.aiCatalog.assignmentsSubtitle', { library: settings.libraryName }) }}</p>
            </div>
          </header>

          <p
            v-if="detailErrorMessage"
            class="rr-admin-ai__detail-error"
          >
            {{ detailErrorMessage }}
          </p>

          <div class="rr-admin-ai__assignment-purpose">
            <span>{{ $t('admin.headers.purpose') }}</span>
            <strong>{{ bindingPurposeLabel(assignmentForm.bindingPurpose) }}</strong>
          </div>

          <div class="rr-admin-ai__form-grid">
            <label class="rr-admin-ai__field rr-admin-ai__field--wide">
              <span>{{ $t('admin.headers.credential') }}</span>
              <select v-model="assignmentForm.providerCredentialId">
                <option
                  v-for="credential in props.settings.credentials"
                  :key="credential.id"
                  :value="credential.id"
                >
                  {{
                    `${credential.label} · ${
                      providerMap.get(credential.providerCatalogId)?.displayName ?? credential.providerCatalogId
                    }`
                  }}
                </option>
              </select>
            </label>

            <label class="rr-admin-ai__field rr-admin-ai__field--wide">
              <span>{{ $t('admin.headers.preset') }}</span>
              <select v-model="assignmentForm.modelPresetId">
                <option
                  v-for="preset in assignmentPresetOptions"
                  :key="preset.id"
                  :value="preset.id"
                >
                  {{ preset.presetName }}
                </option>
              </select>
            </label>

            <label
              v-if="assignmentForm.bindingId"
              class="rr-admin-ai__field"
            >
              <span>{{ $t('admin.headers.state') }}</span>
              <select v-model="assignmentForm.bindingState">
                <option value="active">{{ enumLabel('admin.aiCatalog.bindingStates', 'active') }}</option>
                <option value="invalid">{{ enumLabel('admin.aiCatalog.bindingStates', 'invalid') }}</option>
                <option value="archived">{{ enumLabel('admin.aiCatalog.bindingStates', 'archived') }}</option>
              </select>
            </label>
          </div>

          <div
            v-if="activeValidation"
            class="rr-admin-ai__validation"
          >
            <strong>
              {{ enumLabel('admin.aiCatalog.validationStates', activeValidation.validationState) }}
            </strong>
            <span>{{ formatDateTime(activeValidation.checkedAt) }}</span>
            <span v-if="activeValidation.failureCode">
              {{ activeValidation.failureCode }}
            </span>
            <p v-if="activeValidation.message">{{ activeValidation.message }}</p>
          </div>

          <div class="rr-admin-ai__actions">
            <button
              class="rr-button rr-button--ghost"
              type="button"
              @click="closeDetail"
            >
              {{ $t('dialogs.close') }}
            </button>
            <button
              v-if="assignmentForm.bindingId"
              class="rr-button rr-button--ghost"
              type="button"
              :disabled="validatingBindingId === assignmentForm.bindingId"
              @click="emit('validateBinding', assignmentForm.bindingId)"
            >
              {{
                validatingBindingId === assignmentForm.bindingId
                  ? $t('admin.aiCatalog.validatingBinding')
                  : $t('admin.aiCatalog.validateBinding')
              }}
            </button>
            <button
              class="rr-button"
              type="button"
              :disabled="!canSaveAssignment || saving"
              @click="submitAssignment"
            >
              {{
                assignmentForm.bindingId
                  ? $t('admin.aiCatalog.updateBinding')
                  : $t('admin.aiCatalog.createBinding')
              }}
            </button>
          </div>
        </div>
      </section>
    </div>
  </section>
</template>

<style scoped>
.rr-admin-ai {
  display: grid;
  gap: 0.85rem;
}

.rr-admin-ai__layout {
  display: grid;
  gap: 0.9rem;
  grid-template-columns: 1fr;
  min-height: 0;
}

.rr-admin-ai__layout--editing {
  grid-template-columns: minmax(320px, 0.86fr) minmax(0, 1.14fr);
  min-height: 32rem;
}

.rr-admin-ai__rail,
.rr-admin-ai__detail {
  border: 1px solid var(--rr-border-soft);
  border-radius: 20px;
  background: rgba(255, 255, 255, 0.72);
}

.rr-admin-ai__rail {
  display: grid;
  gap: 0.85rem;
  padding: 0.95rem;
  align-content: start;
}

.rr-admin-ai__intro {
  display: grid;
  gap: 0.35rem;
  padding: 0.9rem 0.95rem;
  border: 1px solid var(--rr-border-muted);
  border-radius: 16px;
  background: rgba(248, 250, 252, 0.78);
}

.rr-admin-ai__intro strong {
  color: var(--rr-text-primary);
  font-size: 0.96rem;
}

.rr-admin-ai__intro p {
  margin: 0;
  color: var(--rr-text-secondary);
  font-size: 0.82rem;
  line-height: 1.5;
}

.rr-admin-ai__rail-group {
  display: grid;
  gap: 0.7rem;
}

.rr-admin-ai__rail-head {
  display: flex;
  justify-content: space-between;
  gap: 0.75rem;
  align-items: flex-start;
}

.rr-admin-ai__rail-head h3,
.rr-admin-ai__detail-head h3 {
  margin: 0;
  font-size: 0.98rem;
  color: var(--rr-text-primary);
}

.rr-admin-ai__rail-head p,
.rr-admin-ai__detail-head p {
  margin: 0.2rem 0 0;
  font-size: 0.84rem;
  line-height: 1.5;
  color: var(--rr-text-secondary);
}

.rr-admin-ai__rail-list {
  display: grid;
  gap: 0.45rem;
}

.rr-admin-ai__rail-row {
  width: 100%;
  border: 1px solid var(--rr-border-muted);
  border-radius: 16px;
  background: rgba(255, 255, 255, 0.78);
  padding: 0.8rem 0.9rem;
  text-align: left;
  display: grid;
  gap: 0.24rem;
  transition:
    border-color 120ms ease,
    background-color 120ms ease,
    transform 120ms ease;
}

.rr-admin-ai__rail-row:hover,
.rr-admin-ai__rail-row--active {
  border-color: rgba(56, 87, 255, 0.18);
  background: rgba(244, 247, 255, 0.96);
}

.rr-admin-ai__rail-row-title {
  font-weight: 600;
  font-size: 0.92rem;
  color: var(--rr-text-primary);
}

.rr-admin-ai__rail-row-meta,
.rr-admin-ai__empty-copy,
.rr-admin-ai__detail-note {
  font-size: 0.82rem;
  line-height: 1.5;
  color: var(--rr-text-secondary);
}

.rr-admin-ai__detail {
  padding: 0.95rem;
}

.rr-admin-ai__detail-empty,
.rr-admin-ai__detail-card {
  height: 100%;
  border: 1px solid var(--rr-border-muted);
  border-radius: 18px;
  background: rgba(248, 250, 252, 0.72);
  padding: 0.95rem;
}

.rr-admin-ai__detail-empty {
  display: grid;
  align-content: center;
  gap: 0.45rem;
  text-align: center;
}

.rr-admin-ai__detail-empty strong {
  color: var(--rr-text-primary);
  font-size: 1rem;
}

.rr-admin-ai__detail-empty p {
  margin: 0;
  color: var(--rr-text-secondary);
}

.rr-admin-ai__detail-card {
  display: grid;
  gap: 1rem;
  align-content: start;
}

.rr-admin-ai__form-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 0.9rem;
}

.rr-admin-ai__field {
  display: grid;
  gap: 0.35rem;
}

.rr-admin-ai__field--wide {
  grid-column: 1 / -1;
}

.rr-admin-ai__field span,
.rr-admin-ai__assignment-purpose span {
  font-size: 0.76rem;
  font-weight: 600;
  color: var(--rr-text-secondary);
}

.rr-admin-ai__field input,
.rr-admin-ai__field select,
.rr-admin-ai__field textarea {
  width: 100%;
  border: 1px solid var(--rr-border-soft);
  border-radius: 14px;
  background: #fff;
  padding: 0.75rem 0.85rem;
  color: var(--rr-text-primary);
}

.rr-admin-ai__field textarea {
  min-height: 9rem;
  resize: vertical;
}

.rr-admin-ai__assignment-purpose {
  display: grid;
  gap: 0.2rem;
  padding: 0.8rem 0.9rem;
  border-radius: 16px;
  background: rgba(255, 255, 255, 0.82);
  border: 1px solid var(--rr-border-muted);
}

.rr-admin-ai__assignment-purpose strong,
.rr-admin-ai__validation strong {
  color: var(--rr-text-primary);
}

.rr-admin-ai__validation {
  display: grid;
  gap: 0.25rem;
  padding: 0.8rem 0.9rem;
  border-radius: 16px;
  background: rgba(241, 245, 249, 0.9);
  border: 1px solid var(--rr-border-muted);
  font-size: 0.84rem;
  color: var(--rr-text-secondary);
}

.rr-admin-ai__validation p {
  margin: 0;
}

.rr-admin-ai__detail-error {
  margin: 0;
  padding: 0.75rem 0.85rem;
  border-radius: 14px;
  background: rgba(254, 242, 242, 0.92);
  border: 1px solid rgba(248, 113, 113, 0.22);
  color: #b91c1c;
  font-size: 0.84rem;
  line-height: 1.45;
}

.rr-admin-ai__actions {
  display: flex;
  flex-wrap: wrap;
  justify-content: flex-end;
  gap: 0.75rem;
}

@media (max-width: 1024px) {
  .rr-admin-ai__layout {
    grid-template-columns: 1fr;
    min-height: 0;
  }

  .rr-admin-ai__form-grid {
    grid-template-columns: 1fr;
  }
}
</style>
