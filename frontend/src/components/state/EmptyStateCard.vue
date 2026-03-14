<script setup lang="ts">
withDefaults(
  defineProps<{
    title: string
    message: string
    hint?: string
  }>(),
  {
    hint: undefined,
  },
)
</script>

<template>
  <article class="state-card state-card--empty">
    <div
      class="state-card__icon"
      aria-hidden="true"
    >
      ○
    </div>
    <div class="state-card__copy">
      <h3>{{ title }}</h3>
      <p>{{ message }}</p>
      <p
        v-if="hint"
        class="state-card__hint"
      >
        {{ hint }}
      </p>
    </div>
    <div
      v-if="$slots.actions"
      class="state-card__actions"
    >
      <slot name="actions" />
    </div>
  </article>
</template>

<style scoped>
.state-card {
  display: flex;
  gap: 14px;
  align-items: flex-start;
  padding: 20px;
  border: 1px dashed #cbd5e1;
  border-radius: 18px;
  background: rgb(255 255 255 / 0.72);
}

.state-card__icon {
  display: grid;
  place-items: center;
  width: 36px;
  height: 36px;
  border-radius: 12px;
  background: #eff6ff;
  color: #2563eb;
  font-weight: 800;
}

.state-card__copy {
  display: grid;
  gap: 6px;
}

.state-card__copy h3,
.state-card__copy p {
  margin: 0;
}

.state-card__copy p {
  color: #526173;
}

.state-card__hint {
  font-size: 0.92rem;
}

.state-card__actions {
  margin-left: auto;
}
</style>
