<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import type {
  AdminPermissionKind,
  CreateApiTokenPayload,
} from 'src/models/ui/admin'

const props = defineProps<{
  open: boolean
  plaintextToken: string | null
  workspaceId: string
  workspaceName: string
  libraryId: string
  libraryName: string
}>()

const emit = defineEmits<{
  close: []
  submit: [payload: CreateApiTokenPayload]
  copy: []
}>()

const label = ref('')
const expiresInDays = ref<string>('90')
const grantResourceKind = ref<'workspace' | 'library'>('library')
const permissionKinds = ref<AdminPermissionKind[]>([])

const workspacePermissionOptions: AdminPermissionKind[] = [
  'workspace_admin',
  'workspace_read',
  'library_read',
  'library_write',
  'document_read',
  'document_write',
  'connector_admin',
  'credential_admin',
  'binding_admin',
  'query_run',
  'ops_read',
  'audit_read',
  'iam_admin',
]

const libraryPermissionOptions: AdminPermissionKind[] = [
  'library_read',
  'library_write',
  'document_read',
  'document_write',
  'connector_admin',
  'binding_admin',
  'query_run',
]

const visiblePermissionOptions = computed(() =>
  grantResourceKind.value === 'workspace'
    ? workspacePermissionOptions
    : libraryPermissionOptions,
)

const selectedScopeName = computed(() =>
  grantResourceKind.value === 'workspace'
    ? props.workspaceName
    : props.libraryName,
)

const canSubmit = computed(
  () => label.value.trim().length > 0 && permissionKinds.value.length > 0,
)

watch(
  () => grantResourceKind.value,
  () => {
    permissionKinds.value = permissionKinds.value.filter((permission) =>
      visiblePermissionOptions.value.includes(permission),
    )
  },
)

watch(
  () => props.open,
  (open) => {
    if (!open) {
      label.value = ''
      expiresInDays.value = '90'
      grantResourceKind.value = 'library'
      permissionKinds.value = []
    }
  },
)

function submit() {
  if (!canSubmit.value) {
    return
  }

  emit('submit', {
    workspaceId: props.workspaceId,
    label: label.value.trim(),
    expiresInDays: expiresInDays.value === 'never' ? null : Number(expiresInDays.value),
    grantResourceKind: grantResourceKind.value,
    grantResourceId:
      grantResourceKind.value === 'workspace' ? props.workspaceId : props.libraryId,
    permissionKinds: permissionKinds.value,
  })
}
</script>

<template>
  <div
    v-if="props.open"
    class="rr-dialog-backdrop"
    @click.self="emit('close')"
  >
    <div class="rr-dialog rr-admin-dialog">
      <template v-if="props.plaintextToken">
        <h3>{{ $t('admin.dialog.revealTitle') }}</h3>
        <p>{{ $t('admin.dialog.revealDescription') }}</p>
        <div class="rr-admin-dialog__token">{{ props.plaintextToken }}</div>
        <div class="rr-dialog__actions">
          <button
            class="rr-button rr-button--ghost"
            type="button"
            @click="emit('close')"
          >
            {{ $t('dialogs.close') }}
          </button>
          <button
            class="rr-button"
            type="button"
            @click="emit('copy')"
          >
            {{ $t('admin.actions.copy') }}
          </button>
        </div>
      </template>
      <template v-else>
        <h3>{{ $t('admin.createToken') }}</h3>
        <p class="rr-admin-dialog__hint">
          {{ $t('admin.dialog.scopeHint', { scope: selectedScopeName }) }}
        </p>

        <div class="rr-field">
          <label for="token-label">{{ $t('admin.dialog.label') }}</label>
          <input
            id="token-label"
            v-model="label"
            type="text"
          >
        </div>

        <div class="rr-field">
          <label for="token-expiry">{{ $t('admin.dialog.expiry') }}</label>
          <select
            id="token-expiry"
            v-model="expiresInDays"
          >
            <option value="30">{{ $t('admin.dialog.dayOption', { count: 30 }) }}</option>
            <option value="90">{{ $t('admin.dialog.dayOption', { count: 90 }) }}</option>
            <option value="365">{{ $t('admin.dialog.dayOption', { count: 365 }) }}</option>
            <option value="never">{{ $t('admin.dialog.never') }}</option>
          </select>
        </div>

        <div class="rr-field">
          <label>{{ $t('admin.dialog.grantScope') }}</label>
          <div class="rr-admin-dialog__scope-switch">
            <label class="rr-form__checkbox">
              <input
                v-model="grantResourceKind"
                type="radio"
                value="library"
              >
              <span>
                {{ $t('admin.dialog.scope.library', { library: props.libraryName }) }}
              </span>
            </label>
            <label class="rr-form__checkbox">
              <input
                v-model="grantResourceKind"
                type="radio"
                value="workspace"
              >
              <span>
                {{ $t('admin.dialog.scope.workspace', { workspace: props.workspaceName }) }}
              </span>
            </label>
          </div>
        </div>

        <div class="rr-field">
          <label>{{ $t('admin.dialog.permissions') }}</label>
          <div class="rr-admin-dialog__scopes">
            <label
              v-for="permission in visiblePermissionOptions"
              :key="permission"
              class="rr-form__checkbox"
            >
              <input
                v-model="permissionKinds"
                type="checkbox"
                :value="permission"
              >
              <span>{{ $t(`admin.tokens.permissions.${permission}`) }}</span>
            </label>
          </div>
        </div>

        <div class="rr-dialog__actions">
          <button
            class="rr-button rr-button--ghost"
            type="button"
            @click="emit('close')"
          >
            {{ $t('dialogs.cancel') }}
          </button>
          <button
            class="rr-button"
            type="button"
            :disabled="!canSubmit"
            @click="submit"
          >
            {{ $t('dialogs.create') }}
          </button>
        </div>
      </template>
    </div>
  </div>
</template>
