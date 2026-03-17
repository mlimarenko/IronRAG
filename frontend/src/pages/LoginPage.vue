<script setup lang="ts">
import { useRouter } from 'vue-router'
import LoginFormCard from 'src/components/auth/LoginFormCard.vue'
import LoginHeroPanel from 'src/components/auth/LoginHeroPanel.vue'
import PublicLayout from 'src/layouts/PublicLayout.vue'
import { useSessionStore } from 'src/stores/session'
import { useShellStore } from 'src/stores/shell'

const router = useRouter()
const sessionStore = useSessionStore()
const shellStore = useShellStore()

async function submit(payload: { login: string; password: string; rememberMe: boolean }) {
  await sessionStore.loginWithPassword({
    login: payload.login,
    password: payload.password,
    rememberMe: payload.rememberMe,
    locale: sessionStore.locale,
  })
  await shellStore.loadContext()
  await router.push('/documents')
}
</script>

<template>
  <PublicLayout>
    <template #hero>
      <LoginHeroPanel />
    </template>

    <LoginFormCard
      :loading="sessionStore.status === 'loading'"
      :error="sessionStore.error"
      @submit="submit"
    />
  </PublicLayout>
</template>
