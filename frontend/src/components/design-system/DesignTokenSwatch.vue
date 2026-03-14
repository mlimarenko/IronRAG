<script setup lang="ts">
defineProps<{
  label: string
  token: string
  value: string
  textColor?: string
}>()
</script>

<template>
  <article class="token-swatch">
    <div
      class="token-swatch__preview"
      :style="{ background: value, color: textColor ?? 'var(--rr-color-text-primary)' }"
    >
      Aa
    </div>
    <div class="token-swatch__meta">
      <strong>{{ label }}</strong>
      <code>{{ token }}</code>
      <span>{{ value }}</span>
    </div>
  </article>
</template>

<style scoped>
.token-swatch {
  display: grid;
  gap: var(--rr-space-3);
}

.token-swatch__preview {
  display: grid;
  place-items: center;
  min-height: 88px;
  border: 1px solid var(--rr-color-border-subtle);
  border-radius: var(--rr-radius-sm);
  font-weight: 700;
}

.token-swatch__meta {
  display: grid;
  gap: 2px;
}

.token-swatch__meta code,
.token-swatch__meta span {
  color: var(--rr-color-text-muted);
  font-size: 0.9rem;
}
</style>
