<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import type { CreateApiTokenPayload } from 'src/models/ui/admin'

const props = defineProps<{
  open: boolean
  plaintextToken: string | null
}>()

const emit = defineEmits<{
  close: []
  submit: [payload: CreateApiTokenPayload]
  copy: []
}>()

const label = ref('')
const expiresInDays = ref<string>('90')
const scopeOptions = [
  'documents:write',
  'documents:read',
  'graph:read',
  'query:read',
  'query:write',
]
const selectedScopes = ref<string[]>(['documents:write', 'graph:read', 'query:write'])

const canSubmit = computed(
  () => label.value.trim().length > 0 && selectedScopes.value.length > 0,
)

watch(
  () => props.open,
  (open) => {
    if (!open) {
      label.value = ''
      expiresInDays.value = '90'
      selectedScopes.value = ['documents:write', 'graph:read', 'query:write']
    }
  },
)

function submit() {
  if (!canSubmit.value) {
    return
  }

  emit('submit', {
    label: label.value.trim(),
    scopes: selectedScopes.value,
    expiresInDays: expiresInDays.value === 'never' ? null : Number(expiresInDays.value),
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
            <option value="30">30 days</option>
            <option value="90">90 days</option>
            <option value="365">365 days</option>
            <option value="never">{{ $t('admin.dialog.never') }}</option>
          </select>
        </div>
        <div class="rr-field">
          <label>{{ $t('admin.dialog.scopes') }}</label>
          <div class="rr-admin-dialog__scopes">
            <label
              v-for="scope in scopeOptions"
              :key="scope"
              class="rr-form__checkbox"
            >
              <input
                v-model="selectedScopes"
                type="checkbox"
                :value="scope"
              >
              <span>{{ scope }}</span>
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
