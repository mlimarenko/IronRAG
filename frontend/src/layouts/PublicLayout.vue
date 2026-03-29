<script setup lang="ts">
import { useSessionStore } from 'src/stores/session'
import LocaleSwitcher from 'src/components/shell/LocaleSwitcher.vue'

const sessionStore = useSessionStore()
</script>

<template>
  <div class="rr-login">
    <slot name="hero" />
    <div class="rr-login__panel">
      <div class="rr-login__panel-header">
        <LocaleSwitcher
          :locale="sessionStore.locale"
          @change="sessionStore.setLocale"
        />
      </div>
      <div class="rr-login__panel-body">
        <slot />
      </div>
    </div>
  </div>
</template>
