<script setup lang="ts">
import { reactive } from 'vue'
import { useI18n } from 'vue-i18n'

const props = defineProps<{
  loading: boolean
  error: string | null
}>()

const emit = defineEmits<{
  submit: [payload: { login: string; password: string; rememberMe: boolean }]
}>()

const { t } = useI18n()
const form = reactive({
  login: '',
  password: '',
  rememberMe: true,
})

function submit() {
  emit('submit', {
    login: form.login,
    password: form.password,
    rememberMe: form.rememberMe,
  })
}
</script>

<template>
  <div class="rr-auth-card">
    <h2>{{ t('auth.title') }}</h2>
    <p>{{ t('auth.subtitle') }}</p>

    <form
      class="rr-form"
      @submit.prevent="submit"
    >
      <div class="rr-field">
        <label for="login">{{ t('auth.login') }}</label>
        <input
          id="login"
          v-model="form.login"
          type="email"
          placeholder="founder@example.local"
          autocomplete="email"
        >
      </div>

      <div class="rr-field">
        <label for="password">{{ t('auth.password') }}</label>
        <input
          id="password"
          v-model="form.password"
          type="password"
          placeholder="••••••••"
          autocomplete="current-password"
        >
      </div>

      <label class="rr-form__checkbox">
        <input
          v-model="form.rememberMe"
          type="checkbox"
        >
        <span>{{ t('auth.remember') }}</span>
      </label>

      <button
        class="rr-button"
        type="submit"
        :disabled="props.loading"
      >
        {{ t('auth.submit') }}
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
