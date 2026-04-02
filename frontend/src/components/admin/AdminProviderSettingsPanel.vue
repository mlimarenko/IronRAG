<script setup lang="ts">
import { computed, reactive, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import SearchField from 'src/components/design-system/SearchField.vue'
import { useDisplayFormatters } from 'src/composables/useDisplayFormatters'
import type {
  AdminAiConsoleState,
  AdminLibraryBinding,
  AdminModelPreset,
  AdminProviderCatalogEntry,
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

type SettingsSection = 'providers' | 'credentials' | 'modelPresets' | 'assignments'
type LibraryTaskPurpose = 'extract_graph' | 'embed_chunk' | 'query_answer' | 'vision'

const LIBRARY_TASKS: LibraryTaskPurpose[] = [
  'extract_graph',
  'embed_chunk',
  'query_answer',
  'vision',
]

const props = defineProps<{
  settings: AdminAiConsoleState
  saving: boolean
  validatingBindingId: string | null
  commitVersion: number
  errorMessage?: string | null
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
const { bindingPurposeLabel, enumLabel, formatDateTime, providerStateLabel } =
  useDisplayFormatters()

const searchQuery = ref('')
const railSection = ref<SettingsSection>('assignments')
const editingSection = ref<SettingsSection | null>(null)
const selectionKey = ref<string | null>(null)
const pendingSubmitSection = ref<SettingsSection | null>(null)
const pendingSelectionKey = ref<string | null>(null)

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
    if (
      !settings.credentials.some(
        (credential) => credential.id === assignmentForm.providerCredentialId,
      )
    ) {
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
    selectionKey.value = null
    editingSection.value = null
    pendingSubmitSection.value = null
    pendingSelectionKey.value = null
    resetCredentialForm()
    resetPresetForm()
    resetAssignmentForm()
  },
)

const providerMap = computed(
  () => new Map(props.settings.providers.map((provider) => [provider.id, provider])),
)

const modelMap = computed(() => new Map(props.settings.models.map((model) => [model.id, model])))

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

const providerCredentialCounts = computed(() => {
  const counts = new Map<string, number>()
  for (const credential of props.settings.credentials) {
    counts.set(credential.providerCatalogId, (counts.get(credential.providerCatalogId) ?? 0) + 1)
  }
  return counts
})

const providerRows = computed(() =>
  props.settings.providers
    .map((provider) => ({
      ...provider,
      modelCount: providerModelCounts.value.get(provider.id) ?? 0,
      credentialCount: providerCredentialCounts.value.get(provider.id) ?? 0,
    }))
    .sort((left, right) => left.displayName.localeCompare(right.displayName)),
)

const credentialRows = computed(() =>
  props.settings.credentials
    .map((credential) => ({
      ...credential,
      provider: providerMap.value.get(credential.providerCatalogId) ?? null,
      providerModelCount: providerModelCounts.value.get(credential.providerCatalogId) ?? 0,
    }))
    .sort((left, right) => left.label.localeCompare(right.label)),
)

const presetRows = computed(() =>
  props.settings.modelPresets
    .map((preset) => {
      const model = modelMap.value.get(preset.modelCatalogId) ?? null
      const provider = model ? (providerMap.value.get(model.providerCatalogId) ?? null) : null
      return { ...preset, model, provider }
    })
    .sort((left, right) => left.presetName.localeCompare(right.presetName)),
)

const assignmentRows = computed(() =>
  props.settings.bindings.map((binding) => {
    const credential =
      props.settings.credentials.find((item) => item.id === binding.providerCredentialId) ?? null
    const preset = presetMap.value.get(binding.modelPresetId) ?? null
    const model = preset ? (modelMap.value.get(preset.modelCatalogId) ?? null) : null
    const provider = credential
      ? (providerMap.value.get(credential.providerCatalogId) ?? null)
      : model
        ? (providerMap.value.get(model.providerCatalogId) ?? null)
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

function modelSupportsBindingPurpose(modelCatalogId: string, purpose: LibraryTaskPurpose): boolean {
  const model = modelMap.value.get(modelCatalogId)
  return !!model && model.allowedBindingPurposes.includes(purpose)
}

const purposeCompatiblePresetOptions = computed(() =>
  props.settings.modelPresets.filter((preset) =>
    modelSupportsBindingPurpose(
      preset.modelCatalogId,
      assignmentForm.bindingPurpose as LibraryTaskPurpose,
    ),
  ),
)

/** Providers that expose at least one catalog model allowed for the selected library task. */
const providerIdsSupportingAssignmentPurpose = computed(() => {
  const purpose = assignmentForm.bindingPurpose as LibraryTaskPurpose
  return new Set(
    props.settings.models
      .filter((model) => model.allowedBindingPurposes.includes(purpose))
      .map((model) => model.providerCatalogId),
  )
})

const assignmentCredentialOptions = computed(() =>
  props.settings.credentials.filter((credential) =>
    providerIdsSupportingAssignmentPurpose.value.has(credential.providerCatalogId),
  ),
)

const assignmentPresetOptions = computed(() => {
  const selectedCredential = assignmentCredentialOptions.value.find(
    (credential) => credential.id === assignmentForm.providerCredentialId,
  )
  if (!selectedCredential) {
    return purposeCompatiblePresetOptions.value
  }
  return purposeCompatiblePresetOptions.value.filter((preset) => {
    const model = modelMap.value.get(preset.modelCatalogId)
    return model?.providerCatalogId === selectedCredential.providerCatalogId
  })
})

const summary = computed(() => ({
  providers: providerRows.value.length,
  credentials: credentialRows.value.length,
  tasks: libraryTaskRows.value.filter((task) => task.binding !== null).length,
}))

const detailErrorMessage = computed(() => props.errorMessage ?? null)

const activeValidation = computed(() => {
  if (!assignmentForm.bindingId) {
    return null
  }
  return (
    assignmentRows.value.find((binding) => binding.id === assignmentForm.bindingId)
      ?.latestValidation ?? null
  )
})

const visibleProviderRows = computed(() =>
  providerRows.value.filter((provider) =>
    matchesQuery([
      provider.displayName,
      provider.providerKind,
      provider.apiStyle,
      provider.lifecycleState,
      provider.modelCount,
      provider.credentialCount,
    ]),
  ),
)

const visibleCredentialRows = computed(() =>
  credentialRows.value.filter((credential) =>
    matchesQuery([
      credential.label,
      credential.provider?.displayName,
      credential.credentialState,
      credential.apiKeySummary,
      credential.providerModelCount,
    ]),
  ),
)

const visiblePresetRows = computed(() =>
  presetRows.value.filter((preset) =>
    matchesQuery([
      preset.presetName,
      preset.provider?.displayName,
      preset.model?.modelName,
      preset.systemPrompt,
    ]),
  ),
)

const visibleTaskRows = computed(() =>
  libraryTaskRows.value.filter((task) =>
    matchesQuery([
      bindingPurposeLabel(task.purpose),
      task.binding?.credential?.label,
      task.binding?.preset?.presetName,
      task.binding?.provider?.displayName,
      task.binding?.latestValidation?.validationState,
      task.binding?.latestValidation?.failureCode,
      task.binding?.latestValidation?.message,
    ]),
  ),
)

const railSections = computed(() => [
  {
    id: 'assignments' as const,
    label: t('admin.aiCatalog.bindingsTitle'),
    subtitle: t('admin.aiCatalog.bindingsSubtitle'),
    count: visibleTaskRows.value.length,
  },
  {
    id: 'credentials' as const,
    label: t('admin.aiCatalog.credentialsTitle'),
    subtitle: t('admin.aiCatalog.credentialsSubtitle'),
    count: visibleCredentialRows.value.length,
  },
  {
    id: 'modelPresets' as const,
    label: t('admin.aiCatalog.modelPresetsTitle'),
    subtitle: t('admin.aiCatalog.modelPresetsSubtitle'),
    count: visiblePresetRows.value.length,
  },
  {
    id: 'providers' as const,
    label: t('admin.aiCatalog.providersTitle'),
    subtitle: t('admin.aiCatalog.providersSubtitle'),
    count: visibleProviderRows.value.length,
  },
])

const activeRailSectionMeta = computed(
  () =>
    railSections.value.find((section) => section.id === railSection.value) ?? railSections.value[0],
)

const activeRailSelectionKeys = computed(() => selectionKeysForSection(railSection.value))

const selectedProvider = computed(() => {
  if (editingSection.value !== 'providers') {
    return null
  }
  const providerId = selectionKey.value?.startsWith('provider:')
    ? selectionKey.value.slice(9)
    : null
  return providerId
    ? (providerRows.value.find((provider) => provider.id === providerId) ?? null)
    : null
})

const selectedProviderModels = computed(() => {
  if (!selectedProvider.value) {
    return []
  }
  return props.settings.models
    .filter((model) => model.providerCatalogId === selectedProvider.value?.id)
    .sort((left, right) => left.modelName.localeCompare(right.modelName))
})

const selectedCredential = computed(() => {
  if (editingSection.value !== 'credentials' || selectionKey.value === 'credential:new') {
    return null
  }
  const credentialId = selectionKey.value?.startsWith('credential:')
    ? selectionKey.value.slice(11)
    : null
  return credentialId
    ? (credentialRows.value.find((credential) => credential.id === credentialId) ?? null)
    : null
})

const selectedPreset = computed(() => {
  if (editingSection.value !== 'modelPresets' || selectionKey.value === 'preset:new') {
    return null
  }
  const presetId = selectionKey.value?.startsWith('preset:') ? selectionKey.value.slice(7) : null
  return presetId ? (presetRows.value.find((preset) => preset.id === presetId) ?? null) : null
})

const selectedTask = computed(() => {
  if (editingSection.value !== 'assignments' || !selectionKey.value) {
    return null
  }
  if (selectionKey.value.startsWith('binding:')) {
    const bindingId = selectionKey.value.slice(8)
    return libraryTaskRows.value.find((task) => task.binding?.id === bindingId) ?? null
  }
  if (selectionKey.value.startsWith('binding-purpose:')) {
    const purpose = selectionKey.value.slice(16) as LibraryTaskPurpose
    return libraryTaskRows.value.find((task) => task.purpose === purpose) ?? null
  }
  return null
})

watch(
  assignmentCredentialOptions,
  (options) => {
    if (options.some((credential) => credential.id === assignmentForm.providerCredentialId)) {
      return
    }
    assignmentForm.providerCredentialId = options[0]?.id ?? ''
  },
  { immediate: true },
)

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

watch(
  activeRailSelectionKeys,
  (keys) => {
    if (
      (railSection.value === 'credentials' && selectionKey.value === 'credential:new') ||
      (railSection.value === 'modelPresets' && selectionKey.value === 'preset:new')
    ) {
      return
    }
    if (keys.length === 0) {
      if (editingSection.value === railSection.value) {
        selectionKey.value = null
        editingSection.value = null
      }
      return
    }
    if (
      editingSection.value !== railSection.value ||
      !selectionKey.value ||
      !keys.includes(selectionKey.value)
    ) {
      activateSelection(keys[0])
    }
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

function matchesQuery(parts: Array<string | number | null | undefined>): boolean {
  const query = searchQuery.value.trim().toLowerCase()
  if (!query) {
    return true
  }
  return parts
    .filter((part) => part !== null && part !== undefined)
    .join(' ')
    .toLowerCase()
    .includes(query)
}

function providerSelectionKey(providerId: string): string {
  return `provider:${providerId}`
}

function credentialSelectionKey(credentialId: string): string {
  return `credential:${credentialId}`
}

function presetSelectionKey(presetId: string): string {
  return `preset:${presetId}`
}

function taskSelectionKey(task: {
  purpose: LibraryTaskPurpose
  binding: AdminLibraryBinding | null
}): string {
  return task.binding ? `binding:${task.binding.id}` : `binding-purpose:${task.purpose}`
}

function selectionKeysForSection(section: SettingsSection): string[] {
  if (section === 'providers') {
    return visibleProviderRows.value.map((provider) => providerSelectionKey(provider.id))
  }
  if (section === 'credentials') {
    return visibleCredentialRows.value.map((credential) => credentialSelectionKey(credential.id))
  }
  if (section === 'modelPresets') {
    return visiblePresetRows.value.map((preset) => presetSelectionKey(preset.id))
  }
  return visibleTaskRows.value.map((task) => taskSelectionKey(task))
}

function showRailSection(section: SettingsSection): void {
  railSection.value = section
}

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

function activateSelection(key: string): void {
  if (key.startsWith('provider:')) {
    const provider = providerRows.value.find((item) => providerSelectionKey(item.id) === key)
    if (provider) {
      selectProvider(provider)
    }
    return
  }
  if (key.startsWith('credential:')) {
    if (key === 'credential:new') {
      openNewCredential()
      return
    }
    const credential = props.settings.credentials.find(
      (item) => credentialSelectionKey(item.id) === key,
    )
    if (credential) {
      selectCredential(credential)
    }
    return
  }
  if (key.startsWith('preset:')) {
    if (key === 'preset:new') {
      openNewPreset()
      return
    }
    const preset = props.settings.modelPresets.find((item) => presetSelectionKey(item.id) === key)
    if (preset) {
      selectPreset(preset)
    }
    return
  }
  if (key.startsWith('binding:') || key.startsWith('binding-purpose:')) {
    const task = libraryTaskRows.value.find((item) => taskSelectionKey(item) === key)
    if (task) {
      selectAssignment(task.purpose)
    }
  }
}

function selectProvider(provider: AdminProviderCatalogEntry): void {
  railSection.value = 'providers'
  editingSection.value = 'providers'
  selectionKey.value = providerSelectionKey(provider.id)
}

function openNewCredential(providerCatalogId?: string): void {
  railSection.value = 'credentials'
  editingSection.value = 'credentials'
  selectionKey.value = 'credential:new'
  resetCredentialForm()
  if (providerCatalogId) {
    credentialForm.providerCatalogId = providerCatalogId
  }
}

function selectCredential(credential?: AdminProviderCredential): void {
  railSection.value = 'credentials'
  editingSection.value = 'credentials'
  selectionKey.value = credential ? credentialSelectionKey(credential.id) : 'credential:new'

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

function openNewPreset(providerCatalogId?: string): void {
  railSection.value = 'modelPresets'
  editingSection.value = 'modelPresets'
  selectionKey.value = 'preset:new'
  resetPresetForm()
  if (providerCatalogId) {
    const model = props.settings.models.find((item) => item.providerCatalogId === providerCatalogId)
    if (model) {
      presetForm.modelCatalogId = model.id
    }
  }
}

function selectPreset(preset?: AdminModelPreset): void {
  railSection.value = 'modelPresets'
  editingSection.value = 'modelPresets'
  selectionKey.value = preset ? presetSelectionKey(preset.id) : 'preset:new'

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
  railSection.value = 'assignments'
  editingSection.value = 'assignments'
  selectionKey.value = binding ? `binding:${binding.id}` : `binding-purpose:${taskPurpose}`

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

function submitCredential(): void {
  if (!canSaveCredential.value) {
    return
  }

  pendingSubmitSection.value = 'credentials'
  pendingSelectionKey.value = selectionKey.value

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
  pendingSelectionKey.value = selectionKey.value

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
  pendingSelectionKey.value = selectionKey.value

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

function providerStatusClass(status: string): string {
  if (status === 'active') {
    return 'is-success'
  }
  if (status === 'invalid') {
    return 'is-danger'
  }
  return 'is-muted'
}

watch(
  () => props.commitVersion,
  (next, previous) => {
    if (next <= previous || !pendingSubmitSection.value) {
      return
    }
    const nextSection = pendingSubmitSection.value
    const nextSelection = pendingSelectionKey.value
    pendingSubmitSection.value = null
    pendingSelectionKey.value = null

    if (nextSection === 'credentials' && nextSelection && nextSelection !== 'credential:new') {
      railSection.value = 'credentials'
      activateSelection(nextSelection)
      return
    }
    if (nextSection === 'modelPresets' && nextSelection && nextSelection !== 'preset:new') {
      railSection.value = 'modelPresets'
      activateSelection(nextSelection)
      return
    }
    if (nextSection === 'assignments' && nextSelection) {
      railSection.value = 'assignments'
      activateSelection(nextSelection)
      return
    }

    if (nextSection === 'credentials' && credentialRows.value.length > 0) {
      selectCredential(props.settings.credentials[0])
      return
    }
    if (nextSection === 'modelPresets' && presetRows.value.length > 0) {
      selectPreset(props.settings.modelPresets[0])
      return
    }
    if (nextSection === 'assignments' && libraryTaskRows.value.length > 0) {
      selectAssignment(libraryTaskRows.value[0].purpose)
    }
  },
)
</script>

<template>
  <section class="rr-admin-workbench rr-admin-workbench--ai">
    <div class="rr-admin-workbench__layout">
      <aside class="rr-admin-workbench__rail">
        <header class="rr-admin-workbench__pane-head">
          <div class="rr-admin-workbench__pane-copy">
            <h3>{{ $t('admin.aiCatalog.title') }}</h3>
            <p>
              {{
                $t('admin.aiCatalog.workspaceSubtitle', {
                  workspace: settings.workspaceName,
                  library: settings.libraryName,
                })
              }}
            </p>
          </div>
        </header>

        <SearchField
          v-model="searchQuery"
          :placeholder="$t('admin.aiCatalog.searchPlaceholder')"
          @clear="searchQuery = ''"
        />

        <div class="rr-admin-workbench__summary">
          <article class="rr-admin-workbench__metric">
            <strong>{{ summary.providers }}</strong>
            <span>{{ $t('admin.aiCatalog.summary.providers') }}</span>
          </article>
          <article class="rr-admin-workbench__metric">
            <strong>{{ summary.credentials }}</strong>
            <span>{{ $t('admin.aiCatalog.summary.credentials') }}</span>
          </article>
          <article class="rr-admin-workbench__metric">
            <strong>{{ summary.tasks }}</strong>
            <span>{{ $t('admin.aiCatalog.summary.tasks') }}</span>
          </article>
        </div>

        <p
          v-if="detailErrorMessage"
          class="rr-admin-workbench__feedback rr-admin-workbench__feedback--error"
        >
          {{ detailErrorMessage }}
        </p>

        <div class="rr-admin-ai-workbench__switcher">
          <button
            v-for="section in railSections"
            :key="section.id"
            type="button"
            class="rr-admin-ai-workbench__switch"
            :class="{ 'rr-admin-ai-workbench__switch--active': railSection === section.id }"
            @click="showRailSection(section.id)"
          >
            <span>{{ section.label }}</span>
            <strong>{{ section.count }}</strong>
          </button>
        </div>

        <section class="rr-admin-ai-workbench__section">
          <header class="rr-admin-ai-workbench__section-head">
            <div class="rr-admin-workbench__pane-copy">
              <h4>{{ activeRailSectionMeta?.label }}</h4>
              <p>{{ activeRailSectionMeta?.subtitle }}</p>
            </div>
            <button
              v-if="railSection === 'credentials'"
              class="rr-button rr-button--ghost rr-button--tiny"
              type="button"
              @click="openNewCredential()"
            >
              {{ $t('admin.aiCatalog.createCredential') }}
            </button>
            <button
              v-else-if="railSection === 'modelPresets'"
              class="rr-button rr-button--ghost rr-button--tiny"
              type="button"
              @click="openNewPreset()"
            >
              {{ $t('admin.aiCatalog.createPreset') }}
            </button>
          </header>

          <div
            v-if="railSection === 'providers' && visibleProviderRows.length"
            class="rr-admin-workbench__group-list"
          >
            <button
              v-for="provider in visibleProviderRows"
              :key="provider.id"
              type="button"
              class="rr-admin-workbench__row"
              :class="{
                'rr-admin-workbench__row--active':
                  selectionKey === providerSelectionKey(provider.id),
              }"
              @click="selectProvider(provider)"
            >
              <div class="rr-admin-workbench__row-head">
                <strong>{{ provider.displayName }}</strong>
                <span class="rr-status-pill" :class="providerStatusClass(provider.lifecycleState)">
                  {{ providerStateLabel(provider.lifecycleState) }}
                </span>
              </div>
              <span class="rr-admin-workbench__row-subtitle">
                {{ enumLabel('admin.aiCatalog.apiStyles', provider.apiStyle) }}
              </span>
              <div class="rr-admin-workbench__row-meta">
                <span>{{
                  t('admin.aiCatalog.providerSummary', {
                    models: provider.modelCount,
                    credentials: provider.credentialCount,
                  })
                }}</span>
              </div>
            </button>
          </div>

          <div
            v-else-if="railSection === 'credentials' && visibleCredentialRows.length"
            class="rr-admin-workbench__group-list"
          >
            <button
              v-for="credential in visibleCredentialRows"
              :key="credential.id"
              type="button"
              class="rr-admin-workbench__row"
              :class="{
                'rr-admin-workbench__row--active':
                  selectionKey === credentialSelectionKey(credential.id),
              }"
              @click="selectCredential(credential)"
            >
              <div class="rr-admin-workbench__row-head">
                <strong>{{ credential.label }}</strong>
                <span
                  class="rr-status-pill"
                  :class="providerStatusClass(credential.credentialState)"
                >
                  {{ enumLabel('admin.aiCatalog.credentialStates', credential.credentialState) }}
                </span>
              </div>
              <span class="rr-admin-workbench__row-subtitle">
                {{ credential.provider?.displayName ?? '—' }}
              </span>
              <div class="rr-admin-workbench__row-meta">
                <span>{{ credential.apiKeySummary }}</span>
                <span>{{ formatDateTime(credential.updatedAt) }}</span>
              </div>
            </button>
          </div>

          <div
            v-else-if="railSection === 'modelPresets' && visiblePresetRows.length"
            class="rr-admin-workbench__group-list"
          >
            <button
              v-for="preset in visiblePresetRows"
              :key="preset.id"
              type="button"
              class="rr-admin-workbench__row"
              :class="{
                'rr-admin-workbench__row--active': selectionKey === presetSelectionKey(preset.id),
              }"
              @click="selectPreset(preset)"
            >
              <div class="rr-admin-workbench__row-head">
                <strong>{{ preset.presetName }}</strong>
                <span class="rr-status-pill is-muted">
                  {{ preset.provider?.displayName ?? '—' }}
                </span>
              </div>
              <span class="rr-admin-workbench__row-subtitle">
                {{ modelDescriptor(preset.modelCatalogId) }}
              </span>
              <div class="rr-admin-workbench__row-meta">
                <span>{{ formatDateTime(preset.updatedAt) }}</span>
              </div>
            </button>
          </div>

          <div
            v-else-if="railSection === 'assignments' && visibleTaskRows.length"
            class="rr-admin-workbench__group-list"
          >
            <button
              v-for="task in visibleTaskRows"
              :key="task.purpose"
              type="button"
              class="rr-admin-workbench__row"
              :class="{
                'rr-admin-workbench__row--active': selectionKey === taskSelectionKey(task),
              }"
              @click="selectAssignment(task.purpose)"
            >
              <div class="rr-admin-workbench__row-head">
                <strong>{{ bindingPurposeLabel(task.purpose) }}</strong>
                <span
                  class="rr-status-pill"
                  :class="
                    task.binding ? providerStatusClass(task.binding.bindingState) : 'is-muted'
                  "
                >
                  {{
                    task.binding
                      ? assignmentValidationLabel(task.binding)
                      : $t('admin.aiCatalog.unsetState')
                  }}
                </span>
              </div>
              <span class="rr-admin-workbench__row-subtitle">
                {{
                  task.binding
                    ? `${task.binding.credential?.label ?? '—'} · ${task.binding.preset?.presetName ?? '—'}`
                    : $t('admin.aiCatalog.unconfiguredTask')
                }}
              </span>
            </button>
          </div>

          <p v-else class="rr-admin-workbench__state">
            {{
              searchQuery
                ? $t('shared.feedbackState.noResults')
                : railSection === 'providers'
                  ? $t('admin.aiCatalog.emptyProviders')
                  : railSection === 'credentials'
                    ? $t('admin.aiCatalog.emptyCredentials')
                    : railSection === 'modelPresets'
                      ? $t('admin.aiCatalog.emptyPresets')
                      : $t('admin.aiCatalog.emptyBindings')
            }}
          </p>
        </section>
      </aside>

      <section class="rr-admin-workbench__detail">
        <div
          v-if="editingSection === 'providers' && selectedProvider"
          class="rr-admin-workbench__detail-card"
        >
          <header class="rr-admin-workbench__detail-head">
            <div class="rr-admin-workbench__pane-copy">
              <h3>{{ selectedProvider.displayName }}</h3>
              <p>{{ enumLabel('admin.aiCatalog.apiStyles', selectedProvider.apiStyle) }}</p>
            </div>
            <span
              class="rr-status-pill"
              :class="providerStatusClass(selectedProvider.lifecycleState)"
            >
              {{ providerStateLabel(selectedProvider.lifecycleState) }}
            </span>
          </header>

          <dl class="rr-admin-workbench__detail-grid">
            <div>
              <dt>{{ $t('admin.headers.provider') }}</dt>
              <dd>{{ selectedProvider.displayName }}</dd>
            </div>
            <div>
              <dt>{{ $t('admin.headers.apiStyle') }}</dt>
              <dd>{{ enumLabel('admin.aiCatalog.apiStyles', selectedProvider.apiStyle) }}</dd>
            </div>
            <div>
              <dt>{{ $t('admin.headers.models') }}</dt>
              <dd>{{ selectedProviderModels.length }}</dd>
            </div>
            <div>
              <dt>{{ $t('admin.aiCatalog.credentialsTitle') }}</dt>
              <dd>{{ providerCredentialCounts.get(selectedProvider.id) ?? 0 }}</dd>
            </div>
          </dl>

          <section class="rr-admin-workbench__detail-section">
            <h4>{{ $t('admin.headers.models') }}</h4>
            <ul class="rr-admin-ai-workbench__model-list">
              <li v-for="model in selectedProviderModels" :key="model.id">
                <strong>{{ model.modelName }}</strong>
                <span>{{ model.capabilityKind }}</span>
              </li>
            </ul>
          </section>

          <div class="rr-admin-workbench__detail-actions">
            <button
              class="rr-button rr-button--ghost"
              type="button"
              @click="openNewCredential(selectedProvider.id)"
            >
              {{ $t('admin.aiCatalog.createCredential') }}
            </button>
            <button class="rr-button" type="button" @click="openNewPreset(selectedProvider.id)">
              {{ $t('admin.aiCatalog.createPreset') }}
            </button>
          </div>
        </div>

        <div v-else-if="editingSection === 'credentials'" class="rr-admin-workbench__detail-card">
          <header class="rr-admin-workbench__detail-head">
            <div class="rr-admin-workbench__pane-copy">
              <h3>
                {{
                  credentialForm.credentialId
                    ? $t('admin.aiCatalog.updateCredential')
                    : $t('admin.aiCatalog.createCredential')
                }}
              </h3>
              <p>
                {{
                  selectedCredential?.provider?.displayName ??
                  $t('admin.aiCatalog.credentialsSubtitle')
                }}
              </p>
            </div>
          </header>

          <dl v-if="selectedCredential" class="rr-admin-workbench__detail-grid">
            <div>
              <dt>{{ $t('admin.headers.provider') }}</dt>
              <dd>{{ selectedCredential.provider?.displayName ?? '—' }}</dd>
            </div>
            <div>
              <dt>{{ $t('admin.headers.state') }}</dt>
              <dd>
                {{
                  enumLabel('admin.aiCatalog.credentialStates', selectedCredential.credentialState)
                }}
              </dd>
            </div>
            <div>
              <dt>{{ $t('admin.headers.updated') }}</dt>
              <dd>{{ formatDateTime(selectedCredential.updatedAt) }}</dd>
            </div>
            <div>
              <dt>{{ $t('admin.aiCatalog.secretRef') }}</dt>
              <dd>{{ selectedCredential.apiKeySummary }}</dd>
            </div>
          </dl>

          <p
            v-if="credentialForm.credentialId"
            class="rr-admin-workbench__feedback rr-admin-workbench__feedback--info"
          >
            {{ $t('admin.aiCatalog.apiKeyKeepExistingHint') }}
          </p>

          <div class="rr-admin-ai-workbench__form-grid">
            <label class="rr-admin-ai-workbench__field rr-admin-ai-workbench__field--wide">
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

            <label class="rr-admin-ai-workbench__field rr-admin-ai-workbench__field--wide">
              <span>{{ $t('admin.headers.label') }}</span>
              <input v-model="credentialForm.label" type="text" />
            </label>

            <label class="rr-admin-ai-workbench__field rr-admin-ai-workbench__field--wide">
              <span>{{ $t('admin.aiCatalog.apiKeyLabel') }}</span>
              <input
                v-model="credentialForm.apiKey"
                type="password"
                :placeholder="
                  credentialForm.credentialId
                    ? $t('admin.aiCatalog.apiKeyKeepExistingPlaceholder')
                    : $t('admin.aiCatalog.apiKeyPlaceholder')
                "
              />
            </label>

            <label v-if="credentialForm.credentialId" class="rr-admin-ai-workbench__field">
              <span>{{ $t('admin.headers.state') }}</span>
              <select v-model="credentialForm.credentialState">
                <option value="active">
                  {{ enumLabel('admin.aiCatalog.credentialStates', 'active') }}
                </option>
                <option value="invalid">
                  {{ enumLabel('admin.aiCatalog.credentialStates', 'invalid') }}
                </option>
                <option value="revoked">
                  {{ enumLabel('admin.aiCatalog.credentialStates', 'revoked') }}
                </option>
              </select>
            </label>
          </div>

          <div class="rr-admin-workbench__detail-actions">
            <button class="rr-button rr-button--ghost" type="button" @click="openNewCredential()">
              {{ $t('admin.aiCatalog.createCredential') }}
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

        <div v-else-if="editingSection === 'modelPresets'" class="rr-admin-workbench__detail-card">
          <header class="rr-admin-workbench__detail-head">
            <div class="rr-admin-workbench__pane-copy">
              <h3>
                {{
                  presetForm.presetId
                    ? $t('admin.aiCatalog.updatePreset')
                    : $t('admin.aiCatalog.createPreset')
                }}
              </h3>
              <p>
                {{
                  selectedPreset
                    ? modelDescriptor(selectedPreset.modelCatalogId)
                    : $t('admin.aiCatalog.modelPresetsSubtitle')
                }}
              </p>
            </div>
          </header>

          <dl v-if="selectedPreset" class="rr-admin-workbench__detail-grid">
            <div>
              <dt>{{ $t('admin.headers.provider') }}</dt>
              <dd>{{ selectedPreset.provider?.displayName ?? '—' }}</dd>
            </div>
            <div>
              <dt>{{ $t('admin.headers.model') }}</dt>
              <dd>{{ selectedPreset.model?.modelName ?? '—' }}</dd>
            </div>
            <div>
              <dt>{{ $t('admin.headers.updated') }}</dt>
              <dd>{{ formatDateTime(selectedPreset.updatedAt) }}</dd>
            </div>
            <div>
              <dt>{{ $t('admin.aiCatalog.temperature') }}</dt>
              <dd>{{ selectedPreset.temperature ?? '—' }}</dd>
            </div>
          </dl>

          <div class="rr-admin-ai-workbench__form-grid">
            <label class="rr-admin-ai-workbench__field rr-admin-ai-workbench__field--wide">
              <span>{{ $t('admin.headers.model') }}</span>
              <select v-model="presetForm.modelCatalogId">
                <option v-for="model in settings.models" :key="model.id" :value="model.id">
                  {{ modelDescriptor(model.id) }}
                </option>
              </select>
            </label>

            <label class="rr-admin-ai-workbench__field rr-admin-ai-workbench__field--wide">
              <span>{{ $t('admin.headers.preset') }}</span>
              <input v-model="presetForm.presetName" type="text" />
            </label>

            <label class="rr-admin-ai-workbench__field rr-admin-ai-workbench__field--wide">
              <span>{{ $t('admin.aiCatalog.systemPrompt') }}</span>
              <textarea v-model="presetForm.systemPrompt" />
            </label>

            <label class="rr-admin-ai-workbench__field">
              <span>{{ $t('admin.aiCatalog.temperature') }}</span>
              <input v-model="presetForm.temperature" type="number" step="0.1" />
            </label>

            <label class="rr-admin-ai-workbench__field">
              <span>{{ $t('admin.aiCatalog.topP') }}</span>
              <input v-model="presetForm.topP" type="number" step="0.1" />
            </label>

            <label class="rr-admin-ai-workbench__field">
              <span>{{ $t('admin.aiCatalog.maxOutputTokens') }}</span>
              <input v-model="presetForm.maxOutputTokensOverride" type="number" min="1" />
            </label>
          </div>

          <div class="rr-admin-workbench__detail-actions">
            <button class="rr-button rr-button--ghost" type="button" @click="openNewPreset()">
              {{ $t('admin.aiCatalog.createPreset') }}
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
          v-else-if="editingSection === 'assignments' && selectedTask"
          class="rr-admin-workbench__detail-card"
        >
          <header class="rr-admin-workbench__detail-head">
            <div class="rr-admin-workbench__pane-copy">
              <h3>{{ bindingPurposeLabel(selectedTask.purpose) }}</h3>
              <p>{{ $t('admin.aiCatalog.bindingsSubtitle') }}</p>
            </div>
            <span
              class="rr-status-pill"
              :class="
                selectedTask.binding
                  ? providerStatusClass(selectedTask.binding.bindingState)
                  : 'is-muted'
              "
            >
              {{
                selectedTask.binding
                  ? assignmentValidationLabel(selectedTask.binding)
                  : $t('admin.aiCatalog.unsetState')
              }}
            </span>
          </header>

          <div class="rr-admin-ai-workbench__assignment-summary">
            <article class="rr-admin-ai-workbench__assignment-chip">
              <span>{{ $t('admin.headers.credential') }}</span>
              <strong>{{ selectedTask.binding?.credential?.label ?? '—' }}</strong>
            </article>
            <article class="rr-admin-ai-workbench__assignment-chip">
              <span>{{ $t('admin.headers.preset') }}</span>
              <strong>{{ selectedTask.binding?.preset?.presetName ?? '—' }}</strong>
            </article>
            <article class="rr-admin-ai-workbench__assignment-chip">
              <span>{{ $t('admin.headers.validation') }}</span>
              <strong>
                {{
                  selectedTask.binding
                    ? assignmentValidationLabel(selectedTask.binding)
                    : $t('admin.aiCatalog.unconfiguredTask')
                }}
              </strong>
            </article>
          </div>

          <div class="rr-admin-ai-workbench__form-grid">
            <label class="rr-admin-ai-workbench__field rr-admin-ai-workbench__field--wide">
              <span>{{ $t('admin.headers.credential') }}</span>
              <select v-model="assignmentForm.providerCredentialId">
                <option
                  v-for="credential in assignmentCredentialOptions"
                  :key="credential.id"
                  :value="credential.id"
                >
                  {{
                    `${credential.label} · ${
                      providerMap.get(credential.providerCatalogId)?.displayName ??
                      credential.providerCatalogId
                    }`
                  }}
                </option>
              </select>
            </label>

            <p
              v-if="assignmentCredentialOptions.length > 0 && assignmentPresetOptions.length === 0"
              class="rr-admin-ai-workbench__field--wide rr-admin-ai-workbench__assignment-hint"
            >
              {{ $t('admin.aiCatalog.presetRequiredForTaskHint') }}
            </p>

            <label class="rr-admin-ai-workbench__field rr-admin-ai-workbench__field--wide">
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

            <label v-if="assignmentForm.bindingId" class="rr-admin-ai-workbench__field">
              <span>{{ $t('admin.headers.state') }}</span>
              <select v-model="assignmentForm.bindingState">
                <option value="active">
                  {{ enumLabel('admin.aiCatalog.bindingStates', 'active') }}
                </option>
                <option value="invalid">
                  {{ enumLabel('admin.aiCatalog.bindingStates', 'invalid') }}
                </option>
                <option value="disabled">
                  {{ enumLabel('admin.aiCatalog.bindingStates', 'disabled') }}
                </option>
              </select>
            </label>
          </div>

          <div v-if="activeValidation" class="rr-admin-ai-workbench__validation">
            <strong>{{
              enumLabel('admin.aiCatalog.validationStates', activeValidation.validationState)
            }}</strong>
            <span>{{ formatDateTime(activeValidation.checkedAt) }}</span>
            <span v-if="activeValidation.failureCode">{{ activeValidation.failureCode }}</span>
            <p v-if="activeValidation.message">{{ activeValidation.message }}</p>
          </div>

          <div class="rr-admin-workbench__detail-actions">
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

        <div v-else class="rr-admin-workbench__state rr-admin-workbench__state--detail">
          {{ $t('admin.aiCatalog.editorPromptDescription') }}
        </div>
      </section>
    </div>
  </section>
</template>

<style scoped lang="scss">
.rr-admin-ai-workbench__switcher {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 8px;
}

.rr-admin-ai-workbench__switch {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
  padding: 10px 12px;
  border: 1px solid rgba(226, 232, 240, 0.9);
  border-radius: 14px;
  background: rgba(248, 250, 252, 0.76);
  color: var(--rr-text-secondary);
  cursor: pointer;
  text-align: left;
  transition:
    border-color 120ms ease,
    background-color 120ms ease,
    box-shadow 120ms ease;
}

.rr-admin-ai-workbench__switch:hover {
  border-color: rgba(56, 87, 255, 0.16);
  background: rgba(244, 247, 255, 0.9);
}

.rr-admin-ai-workbench__switch--active {
  border-color: rgba(56, 87, 255, 0.24);
  background: rgba(244, 247, 255, 0.98);
  box-shadow: 0 4px 16px rgba(56, 87, 255, 0.08);
  color: var(--rr-text-primary);
}

.rr-admin-ai-workbench__switch span {
  font-size: 0.78rem;
  font-weight: 600;
  line-height: 1.35;
}

.rr-admin-ai-workbench__switch strong {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  min-width: 1.75rem;
  min-height: 1.75rem;
  padding: 0 0.45rem;
  border-radius: 999px;
  background: rgba(255, 255, 255, 0.88);
  color: var(--rr-text-primary);
  font-size: 0.76rem;
  line-height: 1;
}

.rr-admin-ai-workbench__section {
  display: grid;
  gap: 10px;
}

.rr-admin-ai-workbench__section-head {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 10px;
}

.rr-admin-ai-workbench__section-head h4 {
  margin: 0;
  color: var(--rr-text-primary);
  font-size: 0.92rem;
  line-height: 1.35;
}

.rr-admin-ai-workbench__model-list {
  display: grid;
  gap: 8px;
  margin: 0;
  padding: 0;
  list-style: none;
}

.rr-admin-ai-workbench__model-list li {
  display: flex;
  flex-wrap: wrap;
  gap: 4px 10px;
  padding: 11px 12px;
  border-radius: 14px;
  border: 1px solid rgba(226, 232, 240, 0.82);
  background: rgba(248, 250, 252, 0.82);
}

.rr-admin-ai-workbench__model-list strong {
  color: var(--rr-text-primary);
  font-size: 0.9rem;
}

.rr-admin-ai-workbench__model-list span {
  color: var(--rr-text-secondary);
  font-size: 0.8rem;
}

.rr-admin-ai-workbench__form-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 12px;
}

.rr-admin-ai-workbench__field {
  display: grid;
  gap: 6px;
}

.rr-admin-ai-workbench__field--wide {
  grid-column: 1 / -1;
}

.rr-admin-ai-workbench__field span {
  color: var(--rr-text-secondary);
  font-size: 0.82rem;
  font-weight: 600;
}

.rr-admin-ai-workbench__field input,
.rr-admin-ai-workbench__field select,
.rr-admin-ai-workbench__field textarea {
  width: 100%;
  min-height: 42px;
  padding: 10px 12px;
  border: 1px solid var(--rr-border-soft);
  border-radius: 14px;
  background: #fff;
  color: var(--rr-text-primary);
  font-size: 0.9rem;
}

.rr-admin-ai-workbench__field textarea {
  min-height: 132px;
  resize: vertical;
}

.rr-admin-ai-workbench__field input:focus,
.rr-admin-ai-workbench__field select:focus,
.rr-admin-ai-workbench__field textarea:focus {
  outline: none;
  border-color: var(--rr-accent);
  box-shadow: 0 0 0 3px var(--rr-accent-muted);
}

.rr-admin-ai-workbench__validation {
  display: grid;
  gap: 4px;
  padding: 12px 14px;
  border-radius: 14px;
  border: 1px solid rgba(59, 130, 246, 0.18);
  background: rgba(239, 246, 255, 0.92);
  color: #1d4ed8;
  font-size: 0.84rem;
  line-height: 1.5;
}

.rr-admin-ai-workbench__validation p {
  margin: 0;
}

.rr-admin-ai-workbench__assignment-summary {
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  gap: 10px;
}

.rr-admin-ai-workbench__assignment-chip {
  display: grid;
  gap: 4px;
  padding: 11px 12px;
  border-radius: 14px;
  border: 1px solid rgba(226, 232, 240, 0.82);
  background: rgba(248, 250, 252, 0.78);
}

.rr-admin-ai-workbench__assignment-chip span {
  color: var(--rr-text-muted);
  font-size: 0.72rem;
  font-weight: 700;
  letter-spacing: 0.08em;
  text-transform: uppercase;
}

.rr-admin-ai-workbench__assignment-chip strong {
  color: var(--rr-text-primary);
  font-size: 0.88rem;
  line-height: 1.4;
}

.rr-admin-ai-workbench__assignment-hint {
  margin: 0;
  color: var(--rr-text-secondary);
  font-size: 0.82rem;
  line-height: 1.45;
}

@media (max-width: 900px) {
  .rr-admin-ai-workbench__switcher,
  .rr-admin-ai-workbench__form-grid,
  .rr-admin-ai-workbench__assignment-summary {
    grid-template-columns: 1fr;
  }
}
</style>
