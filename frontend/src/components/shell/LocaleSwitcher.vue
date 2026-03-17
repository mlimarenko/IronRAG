<script setup lang="ts">
defineProps<{
  locale: 'en' | 'ru'
}>()

const emit = defineEmits<{
  change: [locale: 'en' | 'ru']
}>()
</script>

<template>
  <div class="rr-locale-switcher">
    <button
      type="button"
      :class="{ 'is-active': locale === 'en' }"
      @click="emit('change', 'en')"
    >
      🇬🇧 EN
    </button>
    <button
      type="button"
      :class="{ 'is-active': locale === 'ru' }"
      @click="emit('change', 'ru')"
    >
      🇷🇺 RU
    </button>
  </div>
</template>
