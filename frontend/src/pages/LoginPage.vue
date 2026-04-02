<script setup lang="ts">
import { useRouter } from 'vue-router'
import { onMounted } from 'vue'
import BootstrapSetupCard from 'src/components/auth/BootstrapSetupCard.vue'
import LoginFormCard from 'src/components/auth/LoginFormCard.vue'
import LoginHeroPanel from 'src/components/auth/LoginHeroPanel.vue'
import PublicLayout from 'src/layouts/PublicLayout.vue'
import type { BootstrapSetupAiPayload } from 'src/models/ui/auth'
import { useSessionStore } from 'src/stores/session'
import { useShellStore } from 'src/stores/shell'

const router = useRouter()
const sessionStore = useSessionStore()
const shellStore = useShellStore()

onMounted(async () => {
  if (sessionStore.status === 'idle') {
    await sessionStore.restoreSession()
  }
})

async function completeAuthTransition() {
  try {
    await shellStore.loadContext()
  } catch {
    // shell bootstrap failed — user will see shell-level error after navigation
  }
  await router.push('/')
}

async function submit(payload: { login: string; password: string; rememberMe: boolean }) {
  try {
    await sessionStore.loginWithPassword({
      login: payload.login,
      password: payload.password,
      rememberMe: payload.rememberMe,
      locale: sessionStore.locale,
    })
    await completeAuthTransition()
  } catch {
    // store sets its own error state for UI feedback
  }
}

async function setup(payload: {
  login: string
  displayName: string
  password: string
  aiSetup: BootstrapSetupAiPayload | null
}) {
  try {
    await sessionStore.completeBootstrapSetup({
      login: payload.login,
      displayName: payload.displayName,
      password: payload.password,
      locale: sessionStore.locale,
      aiSetup: payload.aiSetup,
    })
    await completeAuthTransition()
  } catch {
    // store sets its own error state for UI feedback
  }
}
</script>

<template>
  <PublicLayout>
    <template #hero>
      <LoginHeroPanel />
    </template>

    <BootstrapSetupCard
      v-if="sessionStore.requiresBootstrapSetup"
      :ai-setup="sessionStore.bootstrapAiSetup"
      :loading="sessionStore.status === 'loading'"
      :error="sessionStore.error"
      @submit="setup"
    />
    <LoginFormCard
      v-else
      :loading="sessionStore.status === 'loading'"
      :error="sessionStore.error"
      @submit="submit"
    />
  </PublicLayout>
</template>
