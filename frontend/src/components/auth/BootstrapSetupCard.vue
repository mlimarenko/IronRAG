<script setup lang="ts">
import { computed, reactive } from 'vue'
import { useI18n } from 'vue-i18n'

const props = defineProps<{
  loading: boolean
  error: string | null
}>()

const emit = defineEmits<{
  submit: [payload: { login: string; displayName: string; password: string }]
}>()

const { t } = useI18n()
const form = reactive({
  login: '',
  displayName: '',
  password: '',
})

const displayNamePlaceholder = computed(() => form.login.trim() || t('auth.setup.displayNamePlaceholder'))

function submit() {
  emit('submit', {
    login: form.login,
    displayName: form.displayName,
    password: form.password,
  })
}
</script>

<template>
  <div class="rr-auth-card">
    <h2>{{ t('auth.setup.title') }}</h2>
    <p>{{ t('auth.setup.subtitle') }}</p>

    <form
      class="rr-form"
      @submit.prevent="submit"
    >
      <div class="rr-field">
        <label for="setup-login">{{ t('auth.login') }}</label>
        <input
          id="setup-login"
          v-model="form.login"
          type="text"
          placeholder="admin"
          autocomplete="username"
        >
      </div>

      <div class="rr-field">
        <label for="setup-display-name">{{ t('auth.setup.displayName') }}</label>
        <input
          id="setup-display-name"
          v-model="form.displayName"
          type="text"
          :placeholder="displayNamePlaceholder"
          autocomplete="name"
        >
      </div>

      <div class="rr-field">
        <label for="setup-password">{{ t('auth.password') }}</label>
        <input
          id="setup-password"
          v-model="form.password"
          type="password"
          placeholder="••••••••"
          autocomplete="new-password"
        >
      </div>

      <p class="rr-form__hint">
        {{ t('auth.setup.hint') }}
      </p>

      <button
        class="rr-button"
        type="submit"
        :disabled="props.loading"
      >
        {{ t('auth.setup.submit') }}
      </button>

      <p
        v-if="props.error"
        class="rr-error-card"
      >
        {{ props.error }}
      </p>
    </form>
  </div>
</template>
